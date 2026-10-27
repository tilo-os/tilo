//! Concrete, content-addressed local-model service.
//!
//! Downloads are restricted to catalog-pinned HTTPS artifacts and resume into
//! a private staging path. Bytes are hashed and atomically promoted beneath
//! their SHA-256 identity; only then can a signed runtime extension and model
//! become eligible for a typed vLLM launch. There is no generic
//! command, URL, tag, or package-install API.
//!
//! A person's own models ([`hub`]) take the same path from the other end:
//! each file of a Hugging Face pick is verified against the hash the Hub
//! publishes before it is promoted, and the signed runtimes serve it.
//!
//! Some signed models run in the process that uses them instead (an ONNX
//! graph a per-user service loads on the CPU). They go through the same
//! verified download and licence record, but nothing is started: once
//! installed, [`installed_in_process_model`] finds the verified object by
//! capability, reading only the store under [`user_model_root`].

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

pub use agent_provider_config::local::InProcessCapability;
use agent_provider_config::local::hub::UserModel;
use agent_provider_config::local::{
    LocalEndpoint, LocalModelRelease, LocalModelStatus, ModelArtifactFormat, PinnedArtifact,
    RuntimeExecution, RuntimeRelease,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use system_extension::{AcceptedExtension, ExtensionKind};
use thiserror::Error;

pub mod catalog;
pub mod developer;
pub mod hub;
pub mod ownership;
pub mod run_observation;
#[cfg(target_os = "linux")]
pub mod speech;
#[cfg(target_os = "linux")]
pub mod supervision;

const STATE_FILE: &str = "state.json";
const MAX_SNAPSHOT_FILES: u32 = 100_000;
const MAX_SNAPSHOT_PATH_BYTES: usize = 1024;

/// The transfer boundary. Implementations must use HTTPS and honour `start`;
/// this crate validates the pinned HTTPS URL before invoking one. The caller
/// provides the already-open private partial file, so transports never choose
/// arbitrary filesystem destinations.
pub trait HttpsTransport {
    /// Validate a transfer pin. The default production transport accepts HTTPS
    /// only; the explicit developer transport also accepts offline receipts.
    /// # Errors
    /// Rejects invalid or unsupported artifact identities.
    fn validate_pin(&self, artifact: &PinnedArtifact) -> Result<(), ServiceError> {
        validate_artifact(artifact)
    }

    /// Append the bytes of `url` from offset `start` to `destination`, never
    /// more than `maximum_bytes`, and return how many were written.
    ///
    /// # Errors
    /// Returns [`ServiceError::PartialTooLarge`] when the server would send
    /// more than `maximum_bytes`, or a transport or I/O error.
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum_bytes: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError>;
}

/// A supervised runtime boundary. `VllmCommand` is data assembled exclusively
/// from verified artifact paths, a signed extension identity, and a service
/// allocated loopback port — never a shell string.
pub trait ProcessSupervisor {
    /// Import a verified OCI archive and prove that the local image carries
    /// the exact signed digest before it becomes launchable.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the import fails or the local
    /// image does not carry `signed_image`'s digest.
    fn install_runtime_image(
        &mut self,
        archive: &Path,
        signed_image: &str,
    ) -> Result<(), ServiceError>;
    /// Prepare a stopped container and return its exact custody before model code runs.
    fn prepare(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError>;
    /// Launch the durably recorded `handle` and wait for exact endpoint health.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the supervisor refuses the
    /// command or the service does not become ready.
    fn start(&mut self, command: &VllmCommand, handle: &ProcessHandle) -> Result<(), ServiceError>;
    /// Capture content-free counters of this exact owned runtime.
    /// Unsupported supervisors report unavailable, never invented zeros.
    fn observe(
        &self,
        _handle: &ProcessHandle,
    ) -> run_observation::Observed<run_observation::RuntimeSample> {
        run_observation::Observed::unavailable("supervisor has no owned-runtime observer")
    }
    /// Stop the process behind `handle`.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the supervisor cannot stop it.
    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessHandle {
    pub id: String,
    /// Absent only in legacy state. The production supervisor refuses unverifiable cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<ownership::ContainerOwnership>,
}

/// An argv-vector representation suitable for a process/container supervisor.
/// The service never joins these into a shell command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VllmCommand {
    /// Digest-pinned OCI image from the signed runtime catalog. It is consumed
    /// by podman; a downloaded artifact blob is never executed directly.
    pub runtime_image: String,
    pub runtime_kind: agent_provider_config::local::RuntimeKind,
    pub args: Vec<String>,
    pub model_path: PathBuf,
    /// Fixed container-visible target generated from the signed model format.
    pub model_container_path: String,
    pub endpoint: LocalEndpoint,
    pub runtime_extension_id: String,
}

/// A desired runtime must have passed `system-extension`'s signed desired-state
/// transaction. The content-addressed runtime artifact itself is the program;
/// a production OCI supervisor may interpret that verified object as an image
/// instead, but callers cannot supply an entrypoint or mutable image tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRuntime {
    pub accepted: AcceptedExtension,
    pub release: RuntimeRelease,
}

impl SignedRuntime {
    /// Check that the accepted extension is a local-model runtime and names
    /// exactly this release and its artifact.
    ///
    /// # Errors
    /// Returns [`ServiceError::RuntimeNotAccepted`] on any mismatch.
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.accepted.signed.manifest.kind != ExtensionKind::LocalModelRuntime {
            return Err(ServiceError::RuntimeNotAccepted);
        }
        if self.accepted.signed.manifest.id != self.release.id {
            return Err(ServiceError::RuntimeNotAccepted);
        }
        if self.accepted.artifact.sha256 != self.release.artifact.sha256
            || self.accepted.artifact.size_bytes != self.release.artifact.size_bytes
        {
            return Err(ServiceError::RuntimeNotAccepted);
        }
        self.release
            .validate_execution()
            .map_err(|_| ServiceError::RuntimeNotAccepted)?;
        Ok(())
    }

    /// Whether this build runs on `architecture` (`x86_64`, `aarch64`): its
    /// signed manifest names that one, or none.
    #[must_use]
    pub fn runs_on(&self, architecture: &str) -> bool {
        self.accepted
            .signed
            .manifest
            .target
            .architecture
            .as_deref()
            .is_none_or(|target| target == architecture)
    }

    /// Whether this build can serve `model` on `architecture`.
    #[must_use]
    pub fn serves(&self, model: &LocalModelRelease, architecture: &str) -> bool {
        self.runs_on(architecture)
            && model.allowed_runtimes.contains(&self.release.kind)
            && self.release.supports_format(model.artifact_format)
    }
}

/// Build bounded argv exclusively from signed runtime/model metadata.
fn runtime_arguments(
    runtime: &RuntimeRelease,
    model: &LocalModelRelease,
    path: &str,
    port: u16,
) -> Result<Vec<String>, ServiceError> {
    runtime
        .validate_execution()
        .map_err(|_| ServiceError::RuntimeNotAccepted)?;
    model
        .validate_shape()
        .map_err(|_| ServiceError::RuntimeNotAccepted)?;
    if port == 0
        || model.id.is_empty()
        || model.id.len() > 128
        || model.id.starts_with('-')
        || !model.id.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b'/')
        })
    {
        return Err(ServiceError::InvalidEndpoint);
    }
    if !runtime.supports_format(model.artifact_format)
        || !model.allowed_runtimes.contains(&runtime.kind)
    {
        return Err(ServiceError::RuntimeNotAccepted);
    }
    let mut args = match &runtime.execution {
        Some(RuntimeExecution::LlamaCpp {
            max_context_tokens,
            gpu_layers,
            parallel_slots,
        }) => {
            let mut args = vec![
                "--model".into(),
                path.into(),
                "--alias".into(),
                model.id.clone(),
                "--ctx-size".into(),
                (*max_context_tokens).min(model.context_window).to_string(),
                "--n-gpu-layers".into(),
                gpu_layers.to_string(),
                "--jinja".into(),
            ];
            if let Some(slots) = parallel_slots {
                args.extend(["--parallel".into(), slots.get().to_string()]);
            }
            args
        }
        // vLLM's images run `vllm serve`, which takes the model first.
        execution => {
            let mut args = vec![path.into(), "--served-model-name".into(), model.id.clone()];
            if let Some(RuntimeExecution::Vllm {
                max_context_tokens,
                cpu_offload_gib,
            }) = execution
            {
                args.extend([
                    "--max-model-len".into(),
                    (*max_context_tokens).min(model.context_window).to_string(),
                    "--cpu-offload-gb".into(),
                    cpu_offload_gib.to_string(),
                ]);
            }
            args
        }
    };
    args.extend([
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ]);
    Ok(args)
}

/// Persisted non-secret lifecycle state for a model. A process handle is an
/// opaque supervisor reference, not a PID or command line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceState {
    pub models: BTreeMap<String, ManagedModel>,
    /// A non-secret, revision-pinned record of an explicit license decision.
    /// It contains no account identity or token; the catalog's immutable
    /// license id and artifact identity make an old acceptance unusable for a
    /// different release.
    pub accepted_licenses: BTreeMap<String, LicenseAcceptance>,
    /// Models a person picked from Hugging Face or added from a file, by
    /// id: what the signed catalog would otherwise say about them.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub user_models: BTreeMap<String, UserModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModel {
    pub model_sha256: String,
    pub status: LocalModelStatus,
    pub process: Option<ProcessHandle>,
    /// Present only for a model that runs in the process using it. It holds
    /// what [`installed_in_process_model`] answers without the signed catalog,
    /// copied from the release that was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_process: Option<InProcessRecord>,
    /// The content-addressed objects a person's own model was assembled
    /// from, so removal deletes exactly those no other model still uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<String>,
}

/// The installed facts of an in-process model, recorded with its state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InProcessRecord {
    pub revision: String,
    pub license_id: String,
    pub size_bytes: u64,
    pub capabilities: Vec<InProcessCapability>,
    /// When this release was recorded, so the newest of two installed models
    /// with one capability wins.
    pub recorded_at_unix_ms: u64,
}

impl ManagedModel {
    fn for_release(model: &LocalModelRelease, status: LocalModelStatus) -> Self {
        Self {
            model_sha256: model.artifact.sha256.clone(),
            status,
            process: None,
            in_process: model.runs_in_process().then(|| InProcessRecord {
                revision: model.version.clone(),
                license_id: model.license_id.clone(),
                size_bytes: model.artifact.size_bytes,
                capabilities: model.in_process_capabilities(),
                recorded_at_unix_ms: tilo_clock::unix_now_ms(),
            }),
            objects: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledInProcessModel {
    /// Signed catalog id (`u2net-subject`).
    pub id: String,
    /// Immutable signed revision.
    pub revision: String,
    /// SHA-256 of the object at `path`, verified when it was installed.
    pub sha256: String,
    /// The content-addressed object file: `<root>/sha256/<sha256>`.
    pub path: PathBuf,
    /// SPDX licence id the person accepted it under.
    pub license_id: String,
}

/// The per-user model store every Tilo model service shares:
/// `$XDG_DATA_HOME/tilo/models`, or `None` without a data home. There is no
/// shared fallback: a store anyone can write would let them plant the model
/// this user runs.
pub fn user_model_root() -> Option<PathBuf> {
    tilo_paths::xdg::data_home().map(|data| data.join("tilo/models"))
}

/// Find the installed in-process model that provides `capability` in the
/// store at `root` (normally [`user_model_root`]). Reads `state.json` and
/// checks the object only on the filesystem; it never touches the network,
/// the signed catalog or the model manager.
///
/// Only a model whose state is `installed` counts, and only when its object
/// is still a regular file of the recorded size. The bytes were hashed when
/// they were installed and the object is never rewritten, so this does not
/// hash 176 MB on every lookup. With several, the most recently installed
/// wins.
///
/// # Errors
/// Returns an I/O or decode error for an unreadable state file, or
/// [`ServiceError::UnsafeStatePath`] when it is a symlink.
pub fn installed_in_process_model(
    root: &Path,
    capability: InProcessCapability,
) -> Result<Option<InstalledInProcessModel>, ServiceError> {
    let state = load_state(&root.join(STATE_FILE))?;
    let mut found: Option<(u64, InstalledInProcessModel)> = None;
    for (id, managed) in state.models {
        let Some(record) = managed.in_process else {
            continue;
        };
        if managed.status != LocalModelStatus::Installed
            || !record.capabilities.contains(&capability)
            || validate_sha256(&managed.model_sha256).is_err()
        {
            continue;
        }
        let path = root.join("sha256").join(&managed.model_sha256);
        let intact = fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.file_type().is_file() && metadata.len() == record.size_bytes
        });
        if !intact {
            continue;
        }
        if found
            .as_ref()
            .is_some_and(|(recorded, _)| *recorded > record.recorded_at_unix_ms)
        {
            continue;
        }
        found = Some((
            record.recorded_at_unix_ms,
            InstalledInProcessModel {
                id,
                revision: record.revision,
                sha256: managed.model_sha256,
                path,
                license_id: record.license_id,
            },
        ));
    }
    Ok(found.map(|(_, model)| model))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseAcceptance {
    pub model_version: String,
    pub model_sha256: String,
    pub license_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferEvent {
    Received(u64),
    Verifying,
}

/// A background install's progress, published into the state file a
/// status reader polls. The first offset, completion and phase changes are
/// published at once; intermediate byte counts at most every 16 MiB or once
/// a second, so a fast transfer does not fsync per chunk.
struct Progress {
    state_path: PathBuf,
    published: ServiceState,
    persisted: ServiceState,
    model_id: String,
    total: u64,
    last_received: Option<u64>,
    last_publication: Instant,
    error: Option<ServiceError>,
}

impl Progress {
    /// Start from `service`'s state as `prepare` leaves it.
    fn new<T, P>(
        service: &LocalModelService<T, P>,
        model_id: &str,
        total: u64,
        prepare: impl FnOnce(&mut ServiceState),
    ) -> Self {
        let mut published = service.state.clone();
        prepare(&mut published);
        Self {
            state_path: service.root.join(STATE_FILE),
            published,
            persisted: service.persisted.clone(),
            model_id: model_id.to_owned(),
            total,
            last_received: None,
            last_publication: Instant::now(),
            error: None,
        }
    }

    fn event(&mut self, event: TransferEvent) {
        if let Some(managed) = self.published.models.get_mut(&self.model_id) {
            managed.status = match event {
                TransferEvent::Received(received) => LocalModelStatus::Downloading {
                    received_bytes: received,
                    total_bytes: self.total,
                },
                TransferEvent::Verifying => LocalModelStatus::Verifying,
            };
        }
        let publish = match event {
            TransferEvent::Received(received) => {
                self.last_received.is_none()
                    || received == self.total
                    || received.saturating_sub(self.last_received.unwrap_or(0)) >= 16 * 1024 * 1024
                    || self.last_publication.elapsed() >= Duration::from_secs(1)
            }
            TransferEvent::Verifying => true,
        };
        if !publish || self.error.is_some() {
            return;
        }
        match persist_state(&self.state_path, &self.persisted, &self.published) {
            Err(error) => {
                self.error = Some(error);
                return;
            }
            Ok(merged) => {
                self.published = merged;
                self.persisted = self.published.clone();
            }
        }
        if let TransferEvent::Received(received) = event {
            self.last_received = Some(received);
        }
        self.last_publication = Instant::now();
    }

    /// Hand the published state back to `service`. An interrupted transfer
    /// can end between publication thresholds: its last actual byte count
    /// is kept for a resumed worker or status reader before the transfer's
    /// error is returned.
    fn settle<T, P>(
        mut self,
        service: &mut LocalModelService<T, P>,
        transfer: Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if transfer.is_err() && self.error.is_none() {
            self.published = persist_state(&self.state_path, &self.persisted, &self.published)?;
            self.persisted = self.published.clone();
        }
        service.persisted = self.persisted;
        service.state = self.published;
        transfer?;
        self.error.map_or(Ok(()), Err)
    }
}

/// Concrete filesystem service. All storage paths are derived from verified
/// lowercase SHA-256 values, avoiding caller-selected paths and cache aliases.
pub struct LocalModelService<T, P> {
    root: PathBuf,
    transport: T,
    supervisor: P,
    state: ServiceState,
    persisted: ServiceState,
}

impl<T: HttpsTransport, P: ProcessSupervisor> LocalModelService<T, P> {
    /// Open (creating as needed) the model store under `root` and load its
    /// persisted state.
    ///
    /// # Errors
    /// Returns an I/O error when the store directories cannot be created, or
    /// [`ServiceError::UnsafeStatePath`] / a decode error for the state file.
    pub fn open(
        root: impl Into<PathBuf>,
        transport: T,
        supervisor: P,
    ) -> Result<Self, ServiceError> {
        let root = root.into();
        fs::create_dir_all(root.join("sha256"))?;
        fs::create_dir_all(root.join("partial"))?;
        fs::create_dir_all(root.join("snapshots"))?;
        fs::create_dir_all(root.join("snapshot-staging"))?;
        let state = load_state(&root.join(STATE_FILE))?;
        Ok(Self {
            root,
            transport,
            supervisor,
            persisted: state.clone(),
            state,
        })
    }

    pub fn state(&self) -> &ServiceState {
        &self.state
    }

    /// Observe current exact runtime custody without starting or changing it.
    #[must_use]
    pub fn runtime_observations(
        &self,
    ) -> BTreeMap<String, run_observation::Observed<run_observation::RuntimeSample>> {
        self.state
            .models
            .iter()
            .filter_map(|(id, model)| model.process.as_ref().map(|handle| (id, handle)))
            .enumerate()
            .map(|(index, (id, handle))| {
                let observation = if index < 2 {
                    self.fenced_observation(id, handle)
                } else {
                    run_observation::Observed::unavailable(
                        "snapshot live observation count exceeds bound",
                    )
                };
                (id.clone(), observation)
            })
            .collect()
    }

    fn fenced_observation(
        &self,
        model_id: &str,
        handle: &ProcessHandle,
    ) -> run_observation::Observed<run_observation::RuntimeSample> {
        let agrees = || {
            load_state(&self.root.join(STATE_FILE)).is_ok_and(|state| {
                state.models.get(model_id).is_some_and(|model| {
                    model.process.as_ref() == Some(handle)
                        && self
                            .state
                            .models
                            .get(model_id)
                            .is_some_and(|expected| expected.model_sha256 == model.model_sha256)
                })
            })
        };
        if !agrees() {
            return run_observation::Observed::unavailable(
                "runtime state frontier unavailable or changed",
            );
        }
        let sample = self.supervisor.observe(handle);
        if !agrees() {
            return run_observation::Observed::unavailable(
                "runtime state frontier changed during observation",
            );
        }
        sample
    }

    fn checkpoint(
        &self,
        model_id: &str,
        handle: &ProcessHandle,
        phase: &str,
        runtime_id: Option<String>,
    ) -> Option<run_observation::RunCheckpoint> {
        let model = self.state.models.get(model_id)?;
        Some(run_observation::RunCheckpoint {
            schema: 1,
            phase: phase.into(),
            model_id: model_id.into(),
            model_sha256: model.model_sha256.clone(),
            runtime_id,
            process: handle.clone(),
            sample: self.fenced_observation(model_id, handle),
        })
    }

    fn retain_checkpoint(&self, checkpoint: Option<run_observation::RunCheckpoint>) {
        if let Some(checkpoint) = checkpoint
            && run_observation::retain(&self.root, &checkpoint).is_err()
        {
            eprintln!(
                "owned runtime observation checkpoint could not be retained; cleanup is unaffected"
            );
        }
    }

    /// Path of the content-addressed object named by `sha256`.
    ///
    /// # Errors
    /// Returns [`ServiceError::InvalidArtifact`] unless `sha256` is 64
    /// lowercase hex digits.
    pub fn object_path(&self, sha256: &str) -> Result<PathBuf, ServiceError> {
        validate_sha256(sha256)?;
        Ok(self.root.join("sha256").join(sha256))
    }

    fn snapshot_path(&self, sha256: &str) -> Result<PathBuf, ServiceError> {
        validate_sha256(sha256)?;
        Ok(self.root.join("snapshots").join(sha256))
    }

    /// Where the runtime finds installed `model`: the host path it mounts
    /// read-only, the mount's place in the container, and the path the
    /// runtime is told to load.
    fn runtime_mount(
        &self,
        model: &LocalModelRelease,
    ) -> Result<(PathBuf, &'static str, String), ServiceError> {
        let missing = || ServiceError::ModelNotInstalled(model.id.clone());
        if let Some(user) = self.state.user_models.get(&model.id) {
            // A person's own model is a directory of verified files, named
            // as in the repository, so shards find each other.
            let path = self.snapshot_path(&model.artifact.sha256)?;
            if !path.is_dir() {
                return Err(missing());
            }
            let target = if model.artifact_format == ModelArtifactFormat::HfSnapshotFiles {
                validate_snapshot_layout(&path)?;
                "/models/model".to_owned()
            } else {
                format!(
                    "/models/model/{}",
                    user.files.first().ok_or_else(missing)?.path
                )
            };
            return Ok((path, "/models/model", target));
        }
        match model.artifact_format {
            ModelArtifactFormat::HfSnapshotTarZst => {
                let path = self.snapshot_path(&model.artifact.sha256)?;
                if !path.is_dir() {
                    return Err(missing());
                }
                validate_snapshot_layout(&path)?;
                Ok((path, "/models/model", "/models/model".into()))
            }
            ModelArtifactFormat::VllmGguf | ModelArtifactFormat::LlamaCppGguf => {
                let path = self.object_path(&model.artifact.sha256)?;
                if !path.is_file() {
                    return Err(missing());
                }
                Ok((path, "/models/model.gguf", "/models/model.gguf".into()))
            }
            _ => Err(missing()),
        }
    }

    /// Resumes a pinned artifact, verifies its exact digest and size, then
    /// performs an atomic rename into the content-addressed object store.
    pub fn fetch_verified(&mut self, artifact: &PinnedArtifact) -> Result<PathBuf, ServiceError> {
        self.transport.validate_pin(artifact)?;
        let final_path = self.object_path(&artifact.sha256)?;
        if final_path.exists() {
            verify_file(&final_path, artifact)?;
            return Ok(final_path);
        }
        let partial = self.partial_path(&artifact.sha256);
        self.resume_into(&artifact.url, &partial, artifact.size_bytes, &mut |_| {})?;
        verify_file(&partial, artifact)?;
        // `rename` is atomic within this service root. A concurrent successful
        // fetch wins; re-verify its final object instead of overwriting it.
        match fs::rename(&partial, &final_path) {
            Ok(()) => Ok(final_path),
            Err(_error) if final_path.exists() => {
                verify_file(&final_path, artifact)?;
                let _ = fs::remove_file(&partial);
                Ok(final_path)
            }
            Err(error) => Err(ServiceError::Io(error)),
        }
    }

    fn partial_path(&self, name: &str) -> PathBuf {
        self.root.join("partial").join(format!("{name}.part"))
    }

    /// Append `url`'s remaining bytes to the private `partial` file until it
    /// holds exactly `size` bytes, reporting each confirmed byte count. The
    /// caller verifies the bytes before they leave the partial store.
    fn resume_into(
        &mut self,
        url: &str,
        partial: &Path,
        size: u64,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), ServiceError> {
        let start = fs::metadata(partial).map_or(0, |metadata| metadata.len());
        if start > size {
            return Err(ServiceError::PartialTooLarge);
        }
        progress(start);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(partial)?;
        let maximum = size
            .checked_sub(start)
            .ok_or(ServiceError::PartialTooLarge)?;
        let mut reported = start;
        let mut relay = |written: u64| {
            reported = start.saturating_add(written);
            progress(reported);
        };
        let received = if maximum == 0 {
            0
        } else {
            self.transport
                .resume(url, start, maximum, &mut relay, &mut file)?
        };
        file.sync_all()?;
        let total = start
            .checked_add(received)
            .ok_or(ServiceError::PartialTooLarge)?;
        if total != size {
            return Err(ServiceError::IncompleteDownload {
                expected: size,
                actual: total,
            });
        }
        if reported != total {
            progress(total);
        }
        Ok(())
    }

    /// Turn a verified compressed Hugging Face snapshot into a private,
    /// content-addressed directory. Archive paths, links, special files,
    /// unexpected expansion, and duplicate names are refused before any
    /// directory becomes visible to Podman.
    fn materialize_model(&mut self, model: &LocalModelRelease) -> Result<PathBuf, ServiceError> {
        // A per-file layout has no single artifact to fetch: only a
        // person's pick has one, installed through [`hub`].
        if model.artifact_format.is_per_file() {
            return Err(ServiceError::InvalidArtifact);
        }
        let object = self.fetch_verified(&model.artifact)?;
        match model.artifact_format {
            ModelArtifactFormat::HfSnapshotFiles | ModelArtifactFormat::LlamaCppGgufShards => {
                Err(ServiceError::InvalidArtifact)
            }
            ModelArtifactFormat::VllmGguf
            | ModelArtifactFormat::LlamaCppGguf
            | ModelArtifactFormat::OnnxFile
            | ModelArtifactFormat::WhisperGgml
            | ModelArtifactFormat::WhistleCact
            | ModelArtifactFormat::SileroGgml => Ok(object),
            ModelArtifactFormat::HfSnapshotTarZst => self.extract_snapshot(&object, model),
        }
    }

    fn extract_snapshot(
        &self,
        archive: &Path,
        model: &LocalModelRelease,
    ) -> Result<PathBuf, ServiceError> {
        let final_path = self.snapshot_path(&model.artifact.sha256)?;
        if final_path.exists() {
            validate_snapshot_layout(&final_path)?;
            return Ok(final_path);
        }
        let staging = self
            .root
            .join("snapshot-staging")
            .join(format!("{}.extract", model.artifact.sha256));
        remove_private_directory(&staging)?;
        fs::create_dir(&staging)?;
        set_private_directory_mode(&staging)?;
        let outcome = extract_snapshot_into(archive, &staging, model.installed_size_bytes);
        if let Err(error) = outcome {
            let _ = remove_private_directory(&staging);
            return Err(error);
        }
        validate_snapshot_layout(&staging)?;
        match fs::rename(&staging, &final_path) {
            Ok(()) => Ok(final_path),
            Err(_error) if final_path.exists() => {
                validate_snapshot_layout(&final_path)?;
                remove_private_directory(&staging)?;
                Ok(final_path)
            }
            Err(error) => Err(ServiceError::Io(error)),
        }
    }

    /// Record a catalog-verified model as available after placing its bytes.
    ///
    /// # Errors
    /// Returns [`ServiceError::LicenseNotAccepted`] when the release needs a
    /// recorded acceptance, or any fetch, unpack or persistence error.
    pub fn install_model(
        &mut self,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.require_process_absent(&model.id)?;
        self.require_license_acceptance(model)?;
        self.materialize_model(model)?;
        self.state.models.insert(
            model.id.clone(),
            ManagedModel::for_release(model, LocalModelStatus::Installed),
        );
        self.persist()?;
        Ok(LocalModelStatus::Installed)
    }

    /// Install the accepted, verified runtime before placing the model, so an
    /// `Installed` model never fails Start merely because its runtime bytes
    /// were omitted from the operation.
    ///
    /// # Errors
    /// Returns an error when the runtime is not accepted, the license is not
    /// accepted, or fetching, importing or installing fails.
    pub fn install_with_runtime(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.install_accepted_runtime(runtime, model)?;
        self.install_model(model)
    }

    /// Background-worker variant that durably publishes bounded transfer
    /// progress, then verification and installation phases. Transfer callbacks
    /// still observe every chunk; intermediate state fsyncs are coalesced.
    ///
    /// # Errors
    /// As [`Self::install_with_runtime`], plus any failure to publish progress.
    pub fn install_with_runtime_progress(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.install_accepted_runtime(runtime, model)?;
        self.install_model_with_progress(model)
    }

    /// Check `runtime` may serve `model` and its licence is accepted, then
    /// fetch and install the verified runtime image.
    fn install_accepted_runtime(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
    ) -> Result<(), ServiceError> {
        runtime.validate()?;
        if !runtime.release.supports_format(model.artifact_format)
            || !model.allowed_runtimes.contains(&runtime.release.kind)
        {
            return Err(ServiceError::RuntimeNotAccepted);
        }
        self.require_license_acceptance(model)?;
        let archive = self.fetch_verified(&runtime.release.artifact)?;
        self.supervisor
            .install_runtime_image(&archive, &runtime.release.oci_image)?;
        Ok(())
    }

    /// Background-worker install of a model that runs in the process using
    /// it: the same verified, resumable download and licence check, with no
    /// runtime image and nothing to start. It ends `Installed`.
    ///
    /// # Errors
    /// Returns [`ServiceError::NotInProcess`] for a runtime-served model, and
    /// otherwise as [`Self::install_with_runtime_progress`].
    pub fn install_in_process_with_progress(
        &mut self,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        if !model.runs_in_process() {
            return Err(ServiceError::NotInProcess(model.id.clone()));
        }
        self.require_license_acceptance(model)?;
        self.install_model_with_progress(model)
    }

    fn install_model_with_progress(
        &mut self,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.require_process_absent(&model.id)?;
        let mut progress = Progress::new(self, &model.id, model.artifact.size_bytes, |state| {
            state.models.entry(model.id.clone()).or_insert_with(|| {
                ManagedModel::for_release(
                    model,
                    LocalModelStatus::Downloading {
                        received_bytes: 0,
                        total_bytes: model.artifact.size_bytes,
                    },
                )
            });
        });
        let transfer =
            self.fetch_verified_with_progress(&model.artifact, &mut |event| progress.event(event));
        progress.settle(self, transfer.map(|_| ()))?;
        if matches!(model.artifact_format, ModelArtifactFormat::HfSnapshotTarZst) {
            let archive = self.object_path(&model.artifact.sha256)?;
            self.extract_snapshot(&archive, model)?;
        }
        self.state.models.insert(
            model.id.clone(),
            ManagedModel::for_release(model, LocalModelStatus::Installed),
        );
        self.persist()?;
        Ok(LocalModelStatus::Installed)
    }

    fn fetch_verified_with_progress(
        &mut self,
        artifact: &PinnedArtifact,
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<PathBuf, ServiceError> {
        self.transport.validate_pin(artifact)?;
        let final_path = self.object_path(&artifact.sha256)?;
        if final_path.exists() {
            progress(TransferEvent::Received(artifact.size_bytes));
            progress(TransferEvent::Verifying);
            verify_file(&final_path, artifact)?;
            return Ok(final_path);
        }
        let partial = self.partial_path(&artifact.sha256);
        self.resume_into(&artifact.url, &partial, artifact.size_bytes, &mut |bytes| {
            progress(TransferEvent::Received(bytes));
        })?;
        progress(TransferEvent::Verifying);
        if let Err(error) = verify_file(&partial, artifact) {
            // Bytes that do not match can never complete: the next try
            // starts over instead of resuming them forever.
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        fs::rename(&partial, &final_path).map_err(ServiceError::Io)?;
        Ok(final_path)
    }

    /// Durable status written before a background worker begins transfer.
    ///
    /// # Errors
    /// Returns [`ServiceError::LicenseNotAccepted`] or a persistence error.
    pub fn queue_install(&mut self, model: &LocalModelRelease) -> Result<(), ServiceError> {
        self.require_process_absent(&model.id)?;
        self.require_license_acceptance(model)?;
        self.state.models.insert(
            model.id.clone(),
            ManagedModel::for_release(
                model,
                LocalModelStatus::Downloading {
                    received_bytes: 0,
                    total_bytes: model.artifact.size_bytes,
                },
            ),
        );
        self.persist()
    }

    /// Capture the user's explicit confirmation against this exact signed
    /// release. This is separate from UI state and survives a
    /// transient worker restart, while a changed catalog release must be
    /// confirmed again.
    ///
    /// # Errors
    /// Returns an error when the state cannot be persisted.
    pub fn record_license_acceptance(
        &mut self,
        model: &LocalModelRelease,
    ) -> Result<(), ServiceError> {
        self.state.accepted_licenses.insert(
            model.id.clone(),
            LicenseAcceptance {
                model_version: model.version.clone(),
                model_sha256: model.artifact.sha256.clone(),
                license_id: model.license_id.clone(),
            },
        );
        self.persist()
    }

    fn require_license_acceptance(&self, model: &LocalModelRelease) -> Result<(), ServiceError> {
        if !model.requires_acceptance {
            return Ok(());
        }
        let accepted = self.state.accepted_licenses.get(&model.id);
        let accepted =
            accepted.ok_or_else(|| ServiceError::LicenseNotAccepted(model.id.clone()))?;
        if accepted.model_version == model.version
            && accepted.model_sha256 == model.artifact.sha256
            && accepted.license_id == model.license_id
        {
            Ok(())
        } else {
            Err(ServiceError::LicenseNotAccepted(model.id.clone()))
        }
    }

    /// Mark `model_id` failed with `message`, retaining any outstanding process custody.
    ///
    /// # Errors
    /// Returns [`ServiceError::ModelNotInstalled`] for an unknown model, or a
    /// persistence error.
    pub fn record_failure(
        &mut self,
        model_id: &str,
        message: impl Into<String>,
    ) -> Result<(), ServiceError> {
        let managed = self
            .state
            .models
            .get_mut(model_id)
            .ok_or_else(|| ServiceError::ModelNotInstalled(model_id.into()))?;
        managed.status = LocalModelStatus::Failed {
            message: message.into(),
        };
        self.persist()
    }

    /// Persist a queued runtime start before the background worker is
    /// launched.  Settings can return to its event loop immediately and
    /// reconstruct progress after it is closed or restarted.
    ///
    /// # Errors
    /// Returns [`ServiceError::ModelNotInstalled`] for an unknown model, or a
    /// persistence error.
    pub fn mark_starting(&mut self, model_id: &str) -> Result<(), ServiceError> {
        self.require_process_absent(model_id)?;
        if self
            .state
            .models
            .get(model_id)
            .is_some_and(|model| model.status == LocalModelStatus::Starting)
        {
            return Err(ServiceError::Backend(
                "a local model start is already queued".into(),
            ));
        }
        self.update_status(model_id, LocalModelStatus::Starting, None)
    }

    /// Start vLLM under a supervisor. Runtime/model paths are content addressed
    /// and the runtime must have an accepted signed extension record.
    pub fn start(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
        port: u16,
    ) -> Result<LocalEndpoint, ServiceError> {
        if model.runs_in_process() {
            return Err(ServiceError::InProcessModel(model.id.clone()));
        }
        self.require_process_absent(&model.id)?;
        runtime.validate()?;
        if !runtime.release.supports_format(model.artifact_format)
            || !model.allowed_runtimes.contains(&runtime.release.kind)
        {
            return Err(ServiceError::RuntimeNotAccepted);
        }
        if port == 0 {
            return Err(ServiceError::InvalidPort);
        }
        let (model_path, model_container_path, target) = self.runtime_mount(model)?;
        let runtime_path = self.object_path(&runtime.release.artifact.sha256)?;
        if !runtime_path.exists() {
            return Err(ServiceError::RuntimeNotInstalled);
        }
        let endpoint = LocalEndpoint {
            url: format!("http://127.0.0.1:{port}/v1"),
            model_id: model.id.clone(),
        };
        endpoint
            .validate()
            .map_err(|_| ServiceError::InvalidEndpoint)?;
        let command = VllmCommand {
            runtime_image: runtime.release.oci_image.clone(),
            runtime_kind: runtime.release.kind,
            args: runtime_arguments(&runtime.release, model, &target, port)?,
            model_path,
            model_container_path: model_container_path.into(),
            endpoint: endpoint.clone(),
            runtime_extension_id: runtime.accepted.signed.manifest.id.clone(),
        };
        self.update_status(&model.id, LocalModelStatus::Starting, None)?;
        let handle = self.start_prepared(&model.id, &command)?;
        if let Err(error) = self.update_status(
            &model.id,
            LocalModelStatus::Ready {
                endpoint: endpoint.clone(),
            },
            Some(handle.clone()),
        ) {
            return Err(self.failed_start(&model.id, &handle, &error));
        }
        self.retain_checkpoint(self.checkpoint(
            &model.id,
            &handle,
            "ready",
            Some(runtime.release.id.clone()),
        ));
        Ok(endpoint)
    }

    fn require_process_absent(&self, model_id: &str) -> Result<(), ServiceError> {
        let current = load_state(&self.root.join(STATE_FILE))?;
        if current
            .models
            .get(model_id)
            .is_some_and(|model| model.process.is_some())
        {
            return Err(ServiceError::Backend(
                "local model still has runtime custody; stop it before replacing it".into(),
            ));
        }
        Ok(())
    }

    fn start_prepared(
        &mut self,
        model_id: &str,
        command: &VllmCommand,
    ) -> Result<ProcessHandle, ServiceError> {
        let handle = self.supervisor.prepare(command)?;
        if let Err(error) =
            self.update_status(model_id, LocalModelStatus::Starting, Some(handle.clone()))
        {
            // Retain detached custody before attempting cleanup: canonical CAS
            // refusal must never erase the only selector for a prepared container.
            let custody = ownership::retain_orphan(&self.root, model_id, &handle);
            let cleanup = self.supervisor.stop(&handle);
            if cleanup.is_ok()
                && let Ok(id) = &custody
            {
                let _ = std::fs::remove_file(
                    self.root.join("orphan-custody").join(format!("{id}.json")),
                );
            }
            return Err(ServiceError::Backend(format!(
                "prepared runtime was not admitted: {error}; detached custody: {custody:?}; cleanup: {cleanup:?}"
            )));
        }
        if let Err(error) = self.supervisor.start(command, &handle) {
            return Err(self.failed_start(model_id, &handle, &error));
        }
        Ok(handle)
    }

    fn failed_start(
        &mut self,
        model_id: &str,
        handle: &ProcessHandle,
        error: &ServiceError,
    ) -> ServiceError {
        let checkpoint = self.checkpoint(model_id, handle, "failed_start", None);
        let cleanup = self.supervisor.stop(handle);
        let retained = cleanup.is_err().then(|| handle.clone());
        let message = format!("runtime start failed: {error}; cleanup: {cleanup:?}");
        let publication = self.update_status(
            model_id,
            LocalModelStatus::Failed {
                message: message.clone(),
            },
            retained,
        );
        self.retain_checkpoint(checkpoint);
        ServiceError::Backend(format!("{message}; state publication: {publication:?}"))
    }

    /// Stop `model_id`'s running process, if any, and mark it installed.
    ///
    /// # Errors
    /// Returns [`ServiceError::ModelNotInstalled`] for an unknown model, or a
    /// supervisor or persistence error.
    pub fn stop(&mut self, model_id: &str) -> Result<LocalModelStatus, ServiceError> {
        let managed = self
            .state
            .models
            .get(model_id)
            .cloned()
            .ok_or_else(|| ServiceError::ModelNotInstalled(model_id.into()))?;
        let checkpoint = managed
            .process
            .as_ref()
            .and_then(|handle| self.checkpoint(model_id, handle, "before_stop", None));
        if let Some(handle) = managed.process {
            if let Err(error) = self.supervisor.stop(&handle) {
                let _ = self.record_failure(model_id, error.to_string());
                self.retain_checkpoint(checkpoint);
                return Err(error);
            }
        } else if matches!(
            managed.status,
            LocalModelStatus::Starting | LocalModelStatus::Ready { .. }
        ) {
            return Err(ServiceError::Backend(
                "runtime state has no observable process custody".into(),
            ));
        }
        let publication = self.update_status(model_id, LocalModelStatus::Installed, None);
        // Disk retention is deliberately after exact cleanup and its state CAS.
        self.retain_checkpoint(checkpoint);
        publication?;
        Ok(LocalModelStatus::Installed)
    }

    /// Stop first, then delete only the exact content-addressed model object.
    /// Shared/runtime objects are never inferred from a model removal.
    ///
    /// # Errors
    /// Returns [`ServiceError::ModelNotInstalled`] for an unknown model, or a
    /// stop, filesystem or persistence error.
    pub fn remove(&mut self, model_id: &str) -> Result<(), ServiceError> {
        let managed = self
            .state
            .models
            .get(model_id)
            .cloned()
            .ok_or_else(|| ServiceError::ModelNotInstalled(model_id.into()))?;
        if managed.process.is_some() {
            let _ = self.stop(model_id)?;
        }
        let path = self.object_path(&managed.model_sha256)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        let snapshot = self.snapshot_path(&managed.model_sha256)?;
        if snapshot.exists() {
            remove_private_directory(&snapshot)?;
        }
        for object in &managed.objects {
            let shared = self.state.models.iter().any(|(id, other)| {
                id != model_id && (other.model_sha256 == *object || other.objects.contains(object))
            });
            let path = self.object_path(object)?;
            if !shared && path.exists() {
                fs::remove_file(path)?;
            }
        }
        if let Some(user) = self.state.user_models.remove(model_id) {
            self.remove_partials(&user, &managed.model_sha256);
            // The acceptance was of this pick; adding it again asks again.
            self.state.accepted_licenses.remove(model_id);
        }
        self.state.models.remove(model_id);
        self.persist()
    }

    fn update_status(
        &mut self,
        model_id: &str,
        status: LocalModelStatus,
        process: Option<ProcessHandle>,
    ) -> Result<(), ServiceError> {
        let managed = self
            .state
            .models
            .get_mut(model_id)
            .ok_or_else(|| ServiceError::ModelNotInstalled(model_id.into()))?;
        managed.status = status;
        managed.process = process;
        let merged = transact_state(
            &self.root.join(STATE_FILE),
            &self.persisted,
            &self.state,
            Some(model_id),
        )?;
        self.persisted = merged.clone();
        self.state = merged;
        Ok(())
    }

    fn persist(&mut self) -> Result<(), ServiceError> {
        let merged = persist_state(&self.root.join(STATE_FILE), &self.persisted, &self.state)?;
        self.persisted = merged.clone();
        self.state = merged;
        Ok(())
    }
}

fn persist_state(
    path: &Path,
    before: &ServiceState,
    after: &ServiceState,
) -> Result<ServiceState, ServiceError> {
    transact_state(path, before, after, None)
}

fn transact_state(
    path: &Path,
    before: &ServiceState,
    after: &ServiceState,
    required_model: Option<&str>,
) -> Result<ServiceState, ServiceError> {
    let _transaction = ownership::StateFileLock::acquire(path).map_err(ServiceError::Backend)?;
    let mut current = load_state(path)?;
    if let Some(id) = required_model
        && current.models.get(id) != before.models.get(id)
    {
        return Err(ServiceError::Backend(
            "exact local runtime frontier changed".into(),
        ));
    }
    merge_changes(&mut current.models, &before.models, &after.models)?;
    merge_changes(
        &mut current.accepted_licenses,
        &before.accepted_licenses,
        &after.accepted_licenses,
    )?;
    merge_changes(
        &mut current.user_models,
        &before.user_models,
        &after.user_models,
    )?;
    let mut bytes = serde_json::to_vec_pretty(&current)?;
    bytes.push(b'\n');
    tilo_files::native::atomic::write_creating_parent(path, &bytes)?;
    Ok(current)
}

fn merge_changes<T: Clone + PartialEq>(
    current: &mut BTreeMap<String, T>,
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
) -> Result<(), ServiceError> {
    for key in before
        .keys()
        .chain(after.keys().filter(|key| !before.contains_key(*key)))
    {
        if before.get(key) == after.get(key) {
            continue;
        }
        if current.get(key) != before.get(key) {
            return Err(ServiceError::Backend(
                "local model state changed during lifecycle work".into(),
            ));
        }
        if let Some(value) = after.get(key) {
            current.insert(key.clone(), value.clone());
        } else {
            current.remove(key);
        }
    }
    Ok(())
}

fn load_state(path: &Path) -> Result<ServiceState, ServiceError> {
    if !path.exists() {
        return Ok(ServiceState::default());
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(ServiceError::UnsafeStatePath);
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn validate_artifact(artifact: &PinnedArtifact) -> Result<(), ServiceError> {
    if !artifact.url.starts_with("https://") {
        return Err(ServiceError::NonHttpsArtifact);
    }
    validate_sha256(&artifact.sha256)?;
    if artifact.size_bytes == 0 {
        return Err(ServiceError::InvalidArtifact);
    }
    Ok(())
}
fn validate_sha256(value: &str) -> Result<(), ServiceError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(ServiceError::InvalidArtifact)
    }
}
fn verify_file(path: &Path, artifact: &PinnedArtifact) -> Result<(), ServiceError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != artifact.size_bytes {
        return Err(ServiceError::DigestMismatch);
    }
    // Hash the same descriptor whose size was checked. Reading one extra byte
    // detects growth without letting a concurrently extended object turn a
    // pinned receipt into an unbounded read. Large delivery stays constant-space.
    let maximum = artifact
        .size_bytes
        .checked_add(1)
        .ok_or(ServiceError::InvalidArtifact)?;
    let mut reader = (&mut file).take(maximum);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 32 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| ServiceError::DigestMismatch)?)
            .ok_or(ServiceError::DigestMismatch)?;
        hasher.update(buffer.get(..read).ok_or(ServiceError::DigestMismatch)?);
    }
    if total == artifact.size_bytes && hex::encode(hasher.finalize()) == artifact.sha256 {
        Ok(())
    } else {
        Err(ServiceError::DigestMismatch)
    }
}

fn remove_private_directory(path: &Path) -> Result<(), ServiceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(ServiceError::UnsafeSnapshot)
        }
        Ok(_) => {
            fs::remove_dir_all(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ServiceError::Io(error)),
    }
}

fn safe_snapshot_path(path: &Path) -> Result<(), ServiceError> {
    if path.as_os_str().is_empty()
        || path.as_os_str().len() > MAX_SNAPSHOT_PATH_BYTES
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(ServiceError::UnsafeSnapshot)
    } else {
        Ok(())
    }
}

fn extract_snapshot_into(
    archive: &Path,
    destination: &Path,
    maximum_bytes: u64,
) -> Result<(), ServiceError> {
    let file = File::open(archive)?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|error| ServiceError::Archive(error.to_string()))?;
    let mut archive = tar::Archive::new(decoder);
    let mut paths = std::collections::BTreeSet::new();
    let mut files = 0_u32;
    let mut extracted_bytes = 0_u64;
    for entry in archive
        .entries()
        .map_err(|error| ServiceError::Archive(error.to_string()))?
    {
        let mut entry = entry.map_err(|error| ServiceError::Archive(error.to_string()))?;
        let relative = entry
            .path()
            .map_err(|error| ServiceError::Archive(error.to_string()))?
            .into_owned();
        safe_snapshot_path(&relative)?;
        if !paths.insert(relative.clone()) {
            return Err(ServiceError::UnsafeSnapshot);
        }
        files = files.checked_add(1).ok_or(ServiceError::UnsafeSnapshot)?;
        if files > MAX_SNAPSHOT_FILES {
            return Err(ServiceError::UnsafeSnapshot);
        }
        let entry_type = entry.header().entry_type();
        let path = destination.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&path)?;
            set_private_directory_mode(&path)?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(ServiceError::UnsafeSnapshot);
        }
        let declared = entry
            .header()
            .size()
            .map_err(|error| ServiceError::Archive(error.to_string()))?;
        extracted_bytes = extracted_bytes
            .checked_add(declared)
            .ok_or(ServiceError::SnapshotTooLarge)?;
        if extracted_bytes > maximum_bytes {
            return Err(ServiceError::SnapshotTooLarge);
        }
        let parent = path.parent().ok_or(ServiceError::UnsafeSnapshot)?;
        fs::create_dir_all(parent)?;
        set_private_directory_mode(parent)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let written = std::io::copy(&mut entry, &mut output)?;
        if written != declared {
            return Err(ServiceError::Archive("truncated tar entry".into()));
        }
        output.sync_all()?;
        drop(output);
        set_private_file_mode(&path)?;
    }
    if files == 0 {
        return Err(ServiceError::UnsafeSnapshot);
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), ServiceError> {
    // The user service and rootless Podman share this UID. Keep the snapshot
    // private to it while retaining owner write/search permission during
    // extraction and future atomic cleanup.
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<(), ServiceError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(path: &Path) -> Result<(), ServiceError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o400))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_mode(_path: &Path) -> Result<(), ServiceError> {
    Ok(())
}

fn validate_snapshot_layout(path: &Path) -> Result<(), ServiceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ServiceError::UnsafeSnapshot);
    }
    let config = path.join("config.json");
    let tokenizer = path.join("tokenizer.json");
    let tokenizer_config = path.join("tokenizer_config.json");
    if !config.is_file() || (!tokenizer.is_file() && !tokenizer_config.is_file()) {
        return Err(ServiceError::InvalidSnapshotLayout);
    }
    let mut stack = vec![path.to_path_buf()];
    let mut has_weights = false;
    while let Some(directory) = stack.pop() {
        for item in fs::read_dir(directory)? {
            let item = item?;
            let metadata = fs::symlink_metadata(item.path())?;
            if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
                return Err(ServiceError::UnsafeSnapshot);
            }
            if metadata.is_dir() {
                stack.push(item.path());
            } else if item.file_name().to_string_lossy().ends_with(".safetensors")
                || item
                    .file_name()
                    .to_string_lossy()
                    .starts_with("pytorch_model")
                    && item.file_name().to_string_lossy().ends_with(".bin")
            {
                has_weights = true;
            }
        }
    }
    has_weights
        .then_some(())
        .ok_or(ServiceError::InvalidSnapshotLayout)
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("backend error: {0}")]
    Backend(String),
    #[error("artifact is not HTTPS")]
    NonHttpsArtifact,
    #[error("invalid pinned artifact")]
    InvalidArtifact,
    #[error("partial artifact exceeds pinned size")]
    PartialTooLarge,
    #[error("download incomplete: expected {expected} bytes, got {actual}")]
    IncompleteDownload { expected: u64, actual: u64 },
    #[error("artifact digest mismatch")]
    DigestMismatch,
    #[error("unsafe model snapshot archive")]
    UnsafeSnapshot,
    #[error("model snapshot expands beyond its signed installed size")]
    SnapshotTooLarge,
    #[error("model snapshot is missing config, tokenizer, or weights")]
    InvalidSnapshotLayout,
    #[error("model snapshot archive error: {0}")]
    Archive(String),
    #[error("runtime is not an accepted signed local-model extension")]
    RuntimeNotAccepted,
    #[error("runtime artifact is not installed")]
    RuntimeNotInstalled,
    #[error("model is not installed: {0}")]
    ModelNotInstalled(String),
    #[error("model license has not been accepted for this exact release: {0}")]
    LicenseNotAccepted(String),
    #[error("model runs in the process that uses it and is never started: {0}")]
    InProcessModel(String),
    #[error("model is served by a runtime, not loaded in-process: {0}")]
    NotInProcess(String),
    #[error("invalid port")]
    InvalidPort,
    #[error("invalid local endpoint")]
    InvalidEndpoint,
    #[error("unsafe state path")]
    UnsafeStatePath,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::local::RuntimeKind;
    use std::io::{Cursor, Write as _};

    #[test]
    fn offline_developer_fetch_uses_signed_identity_and_existing_hash_gate() {
        let bytes = b"actual offline fixture";
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("inputs");
        fs::create_dir(&input).unwrap();
        let mut release = model(bytes);
        release.artifact.url = format!("offline:sha256:{}", release.artifact.sha256);
        let payload = catalog::CatalogPayload {
            schema: 1,
            models: vec![release.clone()],
            runtimes: vec![],
            recommendation: release.id.clone(),
        };
        let pin = &release.artifact;
        fs::write(input.join(&pin.sha256), bytes).unwrap();
        let store = dir.path().join("store");
        let mut service = LocalModelService::open(
            &store,
            developer::OfflineTransport::new(&input, &payload).unwrap(),
            Supervisor {
                command: None,
                stopped: vec![],
            },
        )
        .unwrap();
        fs::write(
            store.join("partial").join(format!("{}.part", pin.sha256)),
            &bytes[..3],
        )
        .unwrap();
        let object = service.fetch_verified(pin).unwrap();
        assert_eq!(fs::read(&object).unwrap(), bytes);
        let mut unknown = pin.clone();
        unknown.url = "offline:sha256:unknown".into();
        assert!(service.fetch_verified(&unknown).is_err());
        fs::remove_file(&object).unwrap();
        fs::write(input.join(&pin.sha256), vec![b'x'; bytes.len()]).unwrap();
        assert!(matches!(
            service.fetch_verified(pin),
            Err(ServiceError::DigestMismatch)
        ));
        assert!(!object.exists());
        let mut normal = LocalModelService::open(
            dir.path().join("normal"),
            Transport {
                bytes: bytes.to_vec(),
                starts: vec![],
            },
            Supervisor {
                command: None,
                stopped: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            normal.fetch_verified(pin),
            Err(ServiceError::NonHttpsArtifact)
        ));
        assert!(normal.transport.starts.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn offline_developer_fetch_refuses_links_and_wrong_sizes() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("inputs");
        fs::create_dir(&input).unwrap();
        let bytes = b"source";
        let mut release = model(bytes);
        release.artifact.url = format!("offline:sha256:{}", release.artifact.sha256);
        let pin = release.artifact.clone();
        let payload = catalog::CatalogPayload {
            schema: 1,
            models: vec![release],
            runtimes: vec![],
            recommendation: String::new(),
        };
        let real = dir.path().join("real");
        fs::write(&real, bytes).unwrap();
        symlink(&real, input.join(&pin.sha256)).unwrap();
        let mut service = LocalModelService::open(
            dir.path().join("store"),
            developer::OfflineTransport::new(&input, &payload).unwrap(),
            Supervisor {
                command: None,
                stopped: vec![],
            },
        )
        .unwrap();
        assert!(service.fetch_verified(&pin).is_err());
        fs::remove_file(input.join(&pin.sha256)).unwrap();
        fs::write(input.join(&pin.sha256), b"short").unwrap();
        assert!(matches!(
            service.fetch_verified(&pin),
            Err(ServiceError::InvalidArtifact)
        ));
        fs::write(input.join(&pin.sha256), bytes).unwrap();
        let linked = dir.path().join("linked-inputs");
        symlink(&input, &linked).unwrap();
        let mut linked_service = LocalModelService::open(
            dir.path().join("linked-store"),
            developer::OfflineTransport::new(&linked, &payload).unwrap(),
            Supervisor {
                command: None,
                stopped: vec![],
            },
        )
        .unwrap();
        assert!(linked_service.fetch_verified(&pin).is_err());
        assert!(
            !dir.path()
                .join("linked-store/sha256")
                .join(&pin.sha256)
                .exists()
        );
    }

    #[test]
    fn signed_execution_builds_bounded_engine_argv_and_rejects_layout_mismatch() {
        let mut runtime = RuntimeRelease {
            id: "test-runtime".into(),
            kind: RuntimeKind::NvidiaCuda,
            version: "pinned".into(),
            artifact: artifact(b"runtime"),
            oci_image: format!("registry.example.invalid/runtime@sha256:{}", "a".repeat(64)),
            execution: Some(RuntimeExecution::LlamaCpp {
                max_context_tokens: 16384,
                gpu_layers: 12,
                parallel_slots: None,
            }),
        };
        let mut release = model(b"weights");
        release.context_window = 4096;
        release.allowed_runtimes.insert(RuntimeKind::NvidiaCuda);
        assert!(runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).is_err());
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        release.vision = true;
        assert!(runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).is_err());
        release.vision = false;
        let args = runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).unwrap();
        assert_eq!(
            args,
            [
                "--model",
                "/models/model.gguf",
                "--alias",
                "tiny",
                "--ctx-size",
                "4096",
                "--n-gpu-layers",
                "12",
                "--jinja",
                "--host",
                "127.0.0.1",
                "--port",
                "8000"
            ]
        );
        for slots in [1, 64, 65] {
            runtime.execution = Some(RuntimeExecution::LlamaCpp {
                max_context_tokens: 16384,
                gpu_layers: 12,
                parallel_slots: std::num::NonZeroU16::new(slots),
            });
            let generated = runtime_arguments(&runtime, &release, "/models/model.gguf", 8000);
            if slots == 65 {
                assert!(generated.is_err());
            } else {
                let mut expected = args.clone();
                expected.splice(9..9, ["--parallel".into(), slots.to_string()]);
                assert_eq!(generated.unwrap(), expected);
            }
        }
        runtime.kind = RuntimeKind::Cpu;
        assert!(runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).is_err());
        runtime.execution = Some(RuntimeExecution::Vllm {
            max_context_tokens: 1024,
            cpu_offload_gib: 16,
        });
        release.artifact_format = ModelArtifactFormat::VllmGguf;
        let args = runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).unwrap();
        // The image's entrypoint is `vllm serve`: the model comes first.
        assert_eq!(args.first().map(String::as_str), Some("/models/model.gguf"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--served-model-name", "tiny"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--max-model-len", "1024"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--cpu-offload-gb", "16"])
        );
        for invalid_id in ["--host", "alias,other", "", "bad\0id"] {
            release.id = invalid_id.into();
            assert!(runtime_arguments(&runtime, &release, "/models/model.gguf", 8000).is_err());
        }
    }

    #[test]
    fn service_launches_signed_llama_metadata_through_existing_supervisor() {
        let bytes = b"synthetic model and runtime fixture";
        let mut runtime = signed_fixture_runtime(bytes);
        let mut release = model(bytes);
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        release.context_window = 4096;
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_for(dir.path(), bytes);
        service.install_with_runtime(&runtime, &release).unwrap();
        let endpoint = service.start(&runtime, &release, 9000).unwrap();
        assert_eq!(endpoint.model_id, "tiny");
        assert_eq!(endpoint.url, "http://127.0.0.1:9000/v1");
        let command = service.supervisor.command.as_ref().unwrap();
        assert_eq!(command.runtime_image, runtime.release.oci_image);
        assert_eq!(
            command.model_path,
            service.object_path(&release.artifact.sha256).unwrap()
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["--alias", "tiny"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["--ctx-size", "1024"])
        );
        service.supervisor.command = None;
        runtime.release.execution = None;
        assert!(service.start(&runtime, &release, 9000).is_err());
        assert!(service.supervisor.command.is_none());
    }

    #[test]
    fn owned_runtime_unavailable_observation_and_retention_failure_do_not_prevent_stop() {
        let bytes = b"counter observer failure fixture";
        let runtime = signed_fixture_runtime(bytes);
        let mut release = model(bytes);
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        let root = tempfile::tempdir().unwrap();
        let mut service = service_for(root.path(), bytes);
        service.install_with_runtime(&runtime, &release).unwrap();
        service.start(&runtime, &release, 9000).unwrap();
        assert!(matches!(
            service.runtime_observations().get("tiny"),
            Some(run_observation::Observed::Unavailable { .. })
        ));
        fs::write(root.path().join("run-observations"), b"not a directory").unwrap();
        assert_eq!(service.stop("tiny").unwrap(), LocalModelStatus::Installed);
        assert_eq!(service.supervisor.stopped, ["svc-1"]);
        assert!(
            load_state(&root.path().join(STATE_FILE)).unwrap().models["tiny"]
                .process
                .is_none()
        );
        assert!(matches!(
            run_observation::retained(root.path()),
            run_observation::Observed::Unavailable { .. }
        ));
    }

    #[test]
    fn owned_runtime_observation_refuses_retired_state_frontier() {
        let bytes = b"retired counter observer fixture";
        let runtime = signed_fixture_runtime(bytes);
        let mut release = model(bytes);
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        let root = tempfile::tempdir().unwrap();
        let mut service = service_for(root.path(), bytes);
        service.install_with_runtime(&runtime, &release).unwrap();
        service.start(&runtime, &release, 9000).unwrap();
        let stale = service_for(root.path(), bytes);
        service.stop("tiny").unwrap();
        let observations = stale.runtime_observations();
        assert_eq!(
            observations.get("tiny"),
            Some(&run_observation::Observed::unavailable(
                "runtime state frontier unavailable or changed"
            ))
        );
    }

    pub(crate) fn signed_fixture_runtime(bytes: &[u8]) -> SignedRuntime {
        let item = artifact(bytes);
        let manifest = system_extension::ExtensionManifest {
            schema_version: system_extension::SCHEMA_VERSION,
            id: "test-llama-runtime".into(),
            kind: ExtensionKind::LocalModelRuntime,
            version: "test".into(),
            artifact: system_extension::PinnedArtifact {
                source: item.url.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.size_bytes,
            },
            target: system_extension::ImageConstraint {
                base_image_digest: format!("sha256:{}", "a".repeat(64)),
                architecture: None,
                kernel_abi: None,
            },
            requires_reboot: false,
        };
        let signed = system_extension::SignedExtensionManifest::sign(
            manifest,
            "unit-test-key",
            &ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        )
        .unwrap();
        SignedRuntime {
            accepted: AcceptedExtension {
                signed,
                artifact: system_extension::ArtifactReceipt {
                    sha256: item.sha256.clone(),
                    size_bytes: item.size_bytes,
                },
                committed_revision: 1,
            },
            release: RuntimeRelease {
                id: "test-llama-runtime".into(),
                kind: RuntimeKind::Cpu,
                version: "test".into(),
                artifact: item,
                oci_image: format!("registry.example.invalid/llama@sha256:{}", "a".repeat(64)),
                execution: Some(RuntimeExecution::LlamaCpp {
                    max_context_tokens: 1024,
                    gpu_layers: 0,
                    parallel_slots: None,
                }),
            },
        }
    }

    struct BarrierSupervisor {
        state_path: PathBuf,
        entered: Option<std::sync::mpsc::Sender<()>>,
        resume: Option<std::sync::mpsc::Receiver<()>>,
        fail_start: bool,
        fail_stop: bool,
    }
    impl ProcessSupervisor for BarrierSupervisor {
        fn install_runtime_image(&mut self, _: &Path, _: &str) -> Result<(), ServiceError> {
            Ok(())
        }
        fn prepare(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
            let container = ownership::ContainerOwnership {
                container_id: "a".repeat(64),
                run_id: "b".repeat(64),
                image: command.runtime_image.clone(),
                model_id_sha256: hex::encode(Sha256::digest(command.endpoint.model_id.as_bytes())),
                arguments_sha256: ownership::arguments_digest(&command.args),
            };
            Ok(ProcessHandle {
                id: container.unit(),
                container: Some(container),
            })
        }
        fn start(
            &mut self,
            command: &VllmCommand,
            handle: &ProcessHandle,
        ) -> Result<(), ServiceError> {
            let durable = load_state(&self.state_path)?;
            let model = durable
                .models
                .get(&command.endpoint.model_id)
                .ok_or_else(|| ServiceError::Backend("fixture model missing".into()))?;
            if model.process.as_ref() != Some(handle) || model.status != LocalModelStatus::Starting
            {
                return Err(ServiceError::Backend(
                    "launch attempted before durable custody".into(),
                ));
            }
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
            }
            if let Some(resume) = self.resume.take() {
                resume
                    .recv_timeout(tilo_test_env::hang_bound())
                    .map_err(|_| ServiceError::Backend("test barrier timed out".into()))?;
            }
            if self.fail_start {
                Err(ServiceError::Backend(
                    "observed fixture launch refusal".into(),
                ))
            } else {
                Ok(())
            }
        }
        fn stop(&mut self, _: &ProcessHandle) -> Result<(), ServiceError> {
            if self.fail_stop {
                Err(ServiceError::Backend(
                    "observed fixture cleanup refusal".into(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn owned_runtime_concurrent_stop_prevents_stale_ready_publication() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"barrier runtime fixture";
        let runtime = signed_fixture_runtime(bytes);
        let mut release = model(bytes);
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        release.context_window = 4096;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let mut running = LocalModelService::open(
            dir.path(),
            Transport {
                bytes: bytes.to_vec(),
                starts: vec![],
            },
            BarrierSupervisor {
                state_path: dir.path().join(STATE_FILE),
                entered: Some(entered_tx),
                resume: Some(resume_rx),
                fail_start: false,
                fail_stop: false,
            },
        )
        .unwrap();
        running.install_with_runtime(&runtime, &release).unwrap();
        running.mark_starting(&release.id).unwrap();
        let thread = std::thread::spawn(move || running.start(&runtime, &release, 9000));
        tilo_test_env::recv("the launch to enter", &entered_rx);
        let mut stopped = service_for(dir.path(), bytes);
        let prepared = stopped.state.models["tiny"].process.clone().unwrap();
        assert!(prepared.container.is_some());
        assert!(stopped.mark_starting("tiny").is_err());
        assert_eq!(stopped.stop("tiny").unwrap(), LocalModelStatus::Installed);
        resume_tx.send(()).unwrap();
        assert!(thread.join().unwrap().is_err());
        let observed = load_state(&dir.path().join(STATE_FILE)).unwrap();
        assert_eq!(observed.models["tiny"].status, LocalModelStatus::Installed);
        assert!(observed.models["tiny"].process.is_none());
    }

    #[test]
    fn owned_runtime_failed_launch_and_cleanup_retain_durable_exact_custody() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"failed runtime fixture";
        let runtime = signed_fixture_runtime(bytes);
        let mut release = model(bytes);
        release.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        release.context_window = 4096;
        let mut service = LocalModelService::open(
            dir.path(),
            Transport {
                bytes: bytes.to_vec(),
                starts: vec![],
            },
            BarrierSupervisor {
                state_path: dir.path().join(STATE_FILE),
                entered: None,
                resume: None,
                fail_start: true,
                fail_stop: true,
            },
        )
        .unwrap();
        service.install_with_runtime(&runtime, &release).unwrap();
        assert!(service.start(&runtime, &release, 9000).is_err());
        let handle = load_state(&dir.path().join(STATE_FILE)).unwrap().models["tiny"]
            .process
            .clone()
            .unwrap();
        assert!(handle.container.is_some());
        service
            .record_failure("tiny", "worker recorded failure")
            .unwrap();
        assert_eq!(
            load_state(&dir.path().join(STATE_FILE)).unwrap().models["tiny"].process,
            Some(handle.clone())
        );
        assert!(service.queue_install(&release).is_err());
        assert!(service.mark_starting("tiny").is_err());
        assert!(service.stop("tiny").is_err());
        assert_eq!(
            load_state(&dir.path().join(STATE_FILE)).unwrap().models["tiny"].process,
            Some(handle)
        );
    }

    #[test]
    fn owned_runtime_refused_admission_retains_detached_custody_and_exact_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"orphan custody fixture";
        let mut canonical = service_for(dir.path(), bytes);
        canonical.install_model(&model(bytes)).unwrap();
        let mut stale = LocalModelService::open(
            dir.path(),
            Transport {
                bytes: bytes.to_vec(),
                starts: vec![],
            },
            BarrierSupervisor {
                state_path: dir.path().join(STATE_FILE),
                entered: None,
                resume: None,
                fail_start: true,
                fail_stop: true,
            },
        )
        .unwrap();
        canonical
            .record_failure("tiny", "new canonical frontier")
            .unwrap();
        let command = VllmCommand {
            runtime_image: format!("registry.example/runtime@sha256:{}", "c".repeat(64)),
            runtime_kind: RuntimeKind::Cpu,
            args: vec!["serve".into()],
            model_path: dir.path().join("fixture"),
            model_container_path: "/models/model.gguf".into(),
            endpoint: LocalEndpoint {
                url: "http://127.0.0.1:9000/v1".into(),
                model_id: "tiny".into(),
            },
            runtime_extension_id: "fixture".into(),
        };
        let prior = std::fs::read(dir.path().join(STATE_FILE)).unwrap();
        assert!(stale.start_prepared("tiny", &command).is_err());
        assert_eq!(std::fs::read(dir.path().join(STATE_FILE)).unwrap(), prior);
        let receipt_path = dir
            .path()
            .join("orphan-custody")
            .join(format!("{}.json", "b".repeat(64)));
        assert!(receipt_path.is_file());
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).is_err()
        );
        assert!(receipt_path.is_file());
        let body = std::fs::read(&receipt_path).unwrap();
        std::fs::remove_file(&receipt_path).unwrap();
        let foreign_path = dir.path().join("foreign-receipt.json");
        std::fs::write(&foreign_path, &body).unwrap();
        std::os::unix::fs::symlink(&foreign_path, &receipt_path).unwrap();
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).is_err()
        );
        assert_eq!(std::fs::read(&foreign_path).unwrap(), body);
        std::fs::remove_file(&receipt_path).unwrap();
        std::fs::write(&receipt_path, &body).unwrap();
        std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).is_err()
        );
        std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        changed["process"]["container"]["run_id"] = serde_json::json!("c".repeat(64));
        std::fs::write(&receipt_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).is_err()
        );
        std::fs::write(&receipt_path, &body).unwrap();
        let handle = stale.supervisor.prepare(&command).unwrap();
        canonical.state.models.get_mut("tiny").unwrap().process = Some(handle);
        canonical.persist().unwrap();
        stale.supervisor.fail_stop = false;
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).is_err()
        );
        assert!(receipt_path.is_file());
        canonical.state.models.get_mut("tiny").unwrap().process = None;
        canonical.persist().unwrap();
        let current = std::fs::read(dir.path().join(STATE_FILE)).unwrap();
        ownership::recover_orphan(dir.path(), &mut stale.supervisor, &"b".repeat(64)).unwrap();
        assert!(!receipt_path.exists());
        assert_eq!(std::fs::read(dir.path().join(STATE_FILE)).unwrap(), current);
        assert!(
            ownership::recover_orphan(dir.path(), &mut stale.supervisor, "../foreign").is_err()
        );
    }

    #[test]
    fn owned_runtime_state_transactions_merge_foreign_models_and_refuse_stale_custody() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"merge fixture";
        let mut first = service_for(dir.path(), bytes);
        let mut second = service_for(dir.path(), bytes);
        first.install_model(&model(bytes)).unwrap();
        let mut other = model(bytes);
        other.id = "other".into();
        second.install_model(&other).unwrap();
        assert_eq!(
            load_state(&dir.path().join(STATE_FILE))
                .unwrap()
                .models
                .len(),
            2
        );
        first.state.models.get_mut("tiny").unwrap().process = Some(ProcessHandle {
            id: "retained-legacy".into(),
            container: None,
        });
        first.persist().unwrap();
        assert!(second.record_failure("tiny", "stale worker").is_err());
        let observed = load_state(&dir.path().join(STATE_FILE)).unwrap();
        assert_eq!(observed.models.len(), 2);
        assert_eq!(
            observed.models["tiny"].process.as_ref().unwrap().id,
            "retained-legacy"
        );
        assert_eq!(observed.models["other"].status, LocalModelStatus::Installed);
    }

    pub(crate) fn artifact(bytes: &[u8]) -> PinnedArtifact {
        PinnedArtifact {
            url: "https://catalog.example.invalid/object".into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
        }
    }
    fn model(bytes: &[u8]) -> LocalModelRelease {
        LocalModelRelease {
            id: "tiny".into(),
            label: "Tiny".into(),
            description: "A tiny test model".into(),
            version: "1".into(),
            artifact: artifact(bytes),
            artifact_format: ModelArtifactFormat::VllmGguf,
            license_id: "MIT".into(),
            license_url: "https://catalog.example.invalid/license".into(),
            requires_acceptance: false,
            installed_size_bytes: bytes.len() as u64,
            allowed_runtimes: [RuntimeKind::Cpu].into(),
            required_memory_mib: 1,
            required_accelerator_mib: 0,
            context_window: 1,
            vision: false,
            face_recognition: false,
            segmentation: false,
        }
    }
    fn onnx_model(bytes: &[u8]) -> LocalModelRelease {
        LocalModelRelease {
            id: "subject".into(),
            label: "Subject".into(),
            description: "A tiny segmentation test model".into(),
            version: "c".repeat(64),
            artifact: artifact(bytes),
            artifact_format: ModelArtifactFormat::OnnxFile,
            license_id: "Apache-2.0".into(),
            license_url: "https://catalog.example.invalid/license".into(),
            requires_acceptance: true,
            installed_size_bytes: bytes.len() as u64,
            allowed_runtimes: std::collections::BTreeSet::new(),
            required_memory_mib: 1,
            required_accelerator_mib: 0,
            context_window: 0,
            vision: false,
            face_recognition: false,
            segmentation: true,
        }
    }

    fn snapshot(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
        let mut tar = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, Cursor::new(*bytes))
                .unwrap();
        }
        let encoder = tar.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn snapshot_with_symlink() -> Vec<u8> {
        let encoder = zstd::stream::write::Encoder::new(Vec::new(), 0).unwrap();
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "config.json", "/etc/passwd")
            .unwrap();
        let encoder = tar.into_inner().unwrap();
        encoder.finish().unwrap()
    }
    struct Transport {
        bytes: Vec<u8>,
        starts: Vec<u64>,
    }
    impl HttpsTransport for Transport {
        fn resume(
            &mut self,
            _: &str,
            start: u64,
            _: u64,
            progress: &mut dyn FnMut(u64),
            destination: &mut File,
        ) -> Result<u64, ServiceError> {
            self.starts.push(start);
            let start = usize::try_from(start).map_err(|_| ServiceError::PartialTooLarge)?;
            let tail = self
                .bytes
                .get(start..)
                .ok_or(ServiceError::PartialTooLarge)?;
            destination.write_all(tail)?;
            progress(tail.len() as u64);
            Ok(tail.len() as u64)
        }
    }
    struct ChunkedTransport {
        bytes: Vec<u8>,
    }
    struct ObservedChunkTransport {
        bytes: Vec<u8>,
        state_path: PathBuf,
        observed: Vec<u64>,
        interrupt: bool,
    }
    impl HttpsTransport for ObservedChunkTransport {
        fn resume(
            &mut self,
            _: &str,
            start: u64,
            _: u64,
            progress: &mut dyn FnMut(u64),
            destination: &mut File,
        ) -> Result<u64, ServiceError> {
            let mut written = 0_u64;
            let start = usize::try_from(start).map_err(|_| ServiceError::PartialTooLarge)?;
            for chunk in self.bytes[start..].chunks(32) {
                destination.write_all(chunk)?;
                written = written
                    .checked_add(chunk.len() as u64)
                    .ok_or(ServiceError::PartialTooLarge)?;
                progress(written);
                let state = load_state(&self.state_path)?;
                let LocalModelStatus::Downloading { received_bytes, .. } =
                    state.models.values().next().unwrap().status
                else {
                    return Err(ServiceError::Io(std::io::Error::other(
                        "transfer must remain downloading until verification",
                    )));
                };
                self.observed.push(received_bytes);
                if self.interrupt && written >= 128 {
                    return Err(ServiceError::Io(std::io::Error::other("interrupted")));
                }
            }
            Ok(written)
        }
    }

    #[test]
    fn model_install_coalesces_durable_progress_and_flushes_interruption() {
        for interrupt in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let bytes = vec![37; 32 * 256];
            let release = onnx_model(&bytes);
            let mut service = LocalModelService::open(
                dir.path(),
                ObservedChunkTransport {
                    bytes: bytes.clone(),
                    state_path: dir.path().join(STATE_FILE),
                    observed: Vec::new(),
                    interrupt,
                },
                Supervisor {
                    command: None,
                    stopped: Vec::new(),
                },
            )
            .unwrap();
            service.record_license_acceptance(&release).unwrap();
            service.queue_install(&release).unwrap();
            let result = service.install_in_process_with_progress(&release);
            let state = load_state(&dir.path().join(STATE_FILE)).unwrap();
            if interrupt {
                assert!(result.is_err());
                assert_eq!(
                    state.models[&release.id].status,
                    LocalModelStatus::Downloading {
                        received_bytes: 128,
                        total_bytes: bytes.len() as u64
                    }
                );
                let partial = dir
                    .path()
                    .join("partial")
                    .join(format!("{}.part", release.artifact.sha256));
                assert_eq!(fs::read(partial).unwrap(), bytes[..128]);
                assert!(
                    !service
                        .object_path(&release.artifact.sha256)
                        .unwrap()
                        .exists()
                );
                service.transport.interrupt = false;
                assert_eq!(
                    service.install_in_process_with_progress(&release).unwrap(),
                    LocalModelStatus::Installed
                );
                assert_eq!(
                    fs::read(service.object_path(&release.artifact.sha256).unwrap()).unwrap(),
                    bytes
                );
            } else {
                assert_eq!(result.unwrap(), LocalModelStatus::Installed);
                assert_eq!(
                    state.models[&release.id].status,
                    LocalModelStatus::Installed
                );
                assert_eq!(
                    fs::read(service.object_path(&release.artifact.sha256).unwrap()).unwrap(),
                    bytes
                );
                assert_eq!(
                    service.transport.observed.last(),
                    Some(&(bytes.len() as u64))
                );
                // The production installer receives 256 actual chunk writes;
                // intermediate chunks do not each become durable updates.
                assert!(
                    service
                        .transport
                        .observed
                        .windows(2)
                        .any(|pair| pair[0] == pair[1])
                );
            }
        }
    }
    impl HttpsTransport for ChunkedTransport {
        fn resume(
            &mut self,
            _: &str,
            start: u64,
            _: u64,
            progress: &mut dyn FnMut(u64),
            destination: &mut File,
        ) -> Result<u64, ServiceError> {
            let start = usize::try_from(start).map_err(|_| ServiceError::PartialTooLarge)?;
            let tail = self
                .bytes
                .get(start..)
                .ok_or(ServiceError::PartialTooLarge)?;
            let split = tail.len().min(2);
            destination.write_all(&tail[..split])?;
            progress(split as u64);
            destination.write_all(&tail[split..])?;
            progress(tail.len() as u64);
            Ok(tail.len() as u64)
        }
    }
    pub(crate) struct Supervisor {
        pub(crate) command: Option<VllmCommand>,
        pub(crate) stopped: Vec<String>,
    }
    impl ProcessSupervisor for Supervisor {
        fn install_runtime_image(&mut self, _: &Path, _: &str) -> Result<(), ServiceError> {
            Ok(())
        }
        fn prepare(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
            self.command = Some(command.clone());
            Ok(ProcessHandle {
                id: "svc-1".into(),
                container: None,
            })
        }
        fn start(&mut self, _: &VllmCommand, _: &ProcessHandle) -> Result<(), ServiceError> {
            Ok(())
        }
        fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
            self.stopped.push(handle.id.clone());
            Ok(())
        }
    }

    /// A content-addressed fetch resumes and promotes atomically, and a verified fetch publishes each
    /// confirmed byte count.
    #[test]
    fn model_fetches_resume_and_publish_only_verified_bytes() {
        // Phase: content_addressed_fetch_resumes_and_promotes_atomically.
        {
            let bytes = b"a local model";
            let dir = tempfile::tempdir().unwrap();
            let item = artifact(bytes);
            let partial = dir
                .path()
                .join("partial")
                .join(format!("{}.part", item.sha256));
            fs::create_dir_all(partial.parent().unwrap()).unwrap();
            fs::write(&partial, &bytes[..3]).unwrap();
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes: bytes.to_vec(),
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            let object = service.fetch_verified(&item).unwrap();
            assert_eq!(fs::read(object).unwrap(), bytes);
            assert_eq!(service.transport.starts, [3]);
            assert!(!partial.exists());
        }

        // Phase: verified_fetch_publishes_each_confirmed_byte_count.
        {
            let bytes = b"chunked";
            let dir = tempfile::tempdir().unwrap();
            let item = artifact(bytes);
            let mut service = LocalModelService::open(
                dir.path(),
                ChunkedTransport {
                    bytes: bytes.to_vec(),
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            let mut progress = Vec::new();
            service
                .fetch_verified_with_progress(&item, &mut |event| progress.push(event))
                .unwrap();
            assert_eq!(
                progress,
                vec![
                    TransferEvent::Received(0),
                    TransferEvent::Received(2),
                    TransferEvent::Received(bytes.len() as u64),
                    TransferEvent::Verifying,
                ]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn model_delivery_refuses_cached_links_and_non_files() {
        let bytes = b"pinned object";
        let dir = tempfile::tempdir().unwrap();
        let pin = artifact(bytes);
        let mut service = service_for(dir.path(), bytes);
        let path = service.object_path(&pin.sha256).unwrap();
        let outside = dir.path().join("outside-object");
        fs::write(&outside, bytes).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(service.fetch_verified(&pin).is_err());
        assert!(service.transport.starts.is_empty());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(service.fetch_verified(&pin).is_err());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, bytes).unwrap();
        assert_eq!(service.fetch_verified(&pin).unwrap(), path);
        assert!(service.transport.starts.is_empty());
        fs::write(&path, b"pinned object extra").unwrap();
        assert!(service.fetch_verified(&pin).is_err());
    }

    /// A verified snapshot is safely extracted to a content-addressed directory; the path policy
    /// refuses traversal and absolute names; expansion beyond the signed installed size and symlinks are
    /// refused before promotion.
    #[test]
    fn model_snapshots_extract_only_within_their_signed_shape() {
        // Phase: verified_snapshot_is_safely_extracted_to_a_content_addressed_directory.
        {
            let bytes = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{}"),
                ("model-00001-of-00001.safetensors", b"weights"),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let mut release = model(&bytes);
            release.artifact_format = ModelArtifactFormat::HfSnapshotTarZst;
            release.installed_size_bytes = 1024;
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes,
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            service.install_model(&release).unwrap();
            let materialized = service.snapshot_path(&release.artifact.sha256).unwrap();
            assert!(materialized.is_dir());
            assert_eq!(fs::read(materialized.join("config.json")).unwrap(), b"{}");
            assert!(
                materialized
                    .join("model-00001-of-00001.safetensors")
                    .is_file()
            );
        }

        // Phase: snapshot_path_policy_refuses_traversal_and_absolute_names.
        {
            assert!(matches!(
                safe_snapshot_path(Path::new("../escape.safetensors")),
                Err(ServiceError::UnsafeSnapshot)
            ));
            assert!(matches!(
                safe_snapshot_path(Path::new("/etc/passwd")),
                Err(ServiceError::UnsafeSnapshot)
            ));
        }

        // Phase: snapshot_refuses_expansion_beyond_signed_installed_size.
        {
            let bytes = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{}"),
                ("model.safetensors", b"weights"),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let mut release = model(&bytes);
            release.artifact_format = ModelArtifactFormat::HfSnapshotTarZst;
            release.installed_size_bytes = 4;
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes,
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            assert!(matches!(
                service.install_model(&release),
                Err(ServiceError::SnapshotTooLarge)
            ));
            let snapshot = service.snapshot_path(&release.artifact.sha256).unwrap();
            assert!(!snapshot.exists());
        }

        // Phase: snapshot_refuses_symlinks_before_promotion.
        {
            let bytes = snapshot_with_symlink();
            let dir = tempfile::tempdir().unwrap();
            let mut release = model(&bytes);
            release.artifact_format = ModelArtifactFormat::HfSnapshotTarZst;
            release.installed_size_bytes = 1024;
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes,
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            assert!(matches!(
                service.install_model(&release),
                Err(ServiceError::UnsafeSnapshot)
            ));
            let snapshot = service.snapshot_path(&release.artifact.sha256).unwrap();
            assert!(!snapshot.exists());
        }
    }

    fn service_for(root: &Path, bytes: &[u8]) -> LocalModelService<Transport, Supervisor> {
        LocalModelService::open(
            root,
            Transport {
                bytes: bytes.to_vec(),
                starts: vec![],
            },
            Supervisor {
                command: None,
                stopped: vec![],
            },
        )
        .unwrap()
    }

    #[test]
    fn speech_install_uses_verified_objects_and_never_launches_a_runtime() {
        for format in [
            ModelArtifactFormat::WhisperGgml,
            ModelArtifactFormat::WhistleCact,
            ModelArtifactFormat::SileroGgml,
        ] {
            assert_speech_install(format);
        }
    }

    fn assert_speech_install(format: ModelArtifactFormat) {
        let capability = if format == ModelArtifactFormat::SileroGgml {
            InProcessCapability::SpeechActivity
        } else {
            InProcessCapability::SpeechRecognition
        };
        let bytes = b"synthetic speech graph receipt";
        let dir = tempfile::tempdir().unwrap();
        let mut release = onnx_model(bytes);
        release.id = "speech".into();
        release.artifact_format = format;
        release.segmentation = false;
        let mut service = service_for(dir.path(), bytes);
        assert!(matches!(
            service.install_in_process_with_progress(&release),
            Err(ServiceError::LicenseNotAccepted(_))
        ));
        service.record_license_acceptance(&release).unwrap();
        service.queue_install(&release).unwrap();
        assert_eq!(
            installed_in_process_model(dir.path(), capability).unwrap(),
            None
        );
        assert_eq!(
            service.install_in_process_with_progress(&release).unwrap(),
            LocalModelStatus::Installed
        );
        assert!(service.supervisor.command.is_none());
        assert_eq!(
            installed_in_process_model(dir.path(), InProcessCapability::Segmentation).unwrap(),
            None
        );
        let installed = installed_in_process_model(dir.path(), capability)
            .unwrap()
            .unwrap();
        assert_eq!(installed.id, "speech");
        assert_eq!(installed.sha256, release.artifact.sha256);
        assert_eq!(fs::read(&installed.path).unwrap(), bytes);
        // A test-only trust fixture stays in this temporary directory; it
        // never changes the image keyring or the person's model store.
        #[cfg(target_os = "linux")]
        {
            use crate::catalog::VerifiedCatalog;
            let (verified, signed, catalog_path, keys_path) =
                signed_speech_witness(dir.path(), &release);
            if format == ModelArtifactFormat::SileroGgml {
                service.state.models.get_mut("speech").unwrap().status =
                    LocalModelStatus::NotInstalled;
                service.persist().unwrap();
                assert!(matches!(
                    crate::speech::speech_activity_file(dir.path(), &verified),
                    Err(crate::speech::SpeechError::ModelUnavailable)
                ));
                service.state.models.get_mut("speech").unwrap().status =
                    LocalModelStatus::Installed;
                service.persist().unwrap();
            }
            assert_speech_witness(dir.path(), &verified, format, bytes);
            fs::write(&installed.path, vec![0; bytes.len()]).unwrap();
            assert!(matches!(
                read_speech_witness(dir.path(), &verified, format),
                Err(crate::speech::SpeechError::ModelReceipt)
            ));
            fs::write(&installed.path, bytes).unwrap();
            service.state.accepted_licenses.clear();
            service.persist().unwrap();
            assert!(matches!(
                read_speech_witness(dir.path(), &verified, format),
                Err(crate::speech::SpeechError::ModelUnavailable)
            ));
            service.record_license_acceptance(&release).unwrap();
            fs::remove_file(&installed.path).unwrap();
            std::os::unix::fs::symlink(&catalog_path, &installed.path).unwrap();
            assert!(matches!(
                read_speech_witness(dir.path(), &verified, format),
                Err(crate::speech::SpeechError::ModelUnavailable)
            ));
            fs::remove_file(&installed.path).unwrap();
            fs::write(&installed.path, bytes).unwrap();
            let mut forged = signed;
            forged.payload.models[0].artifact_format = ModelArtifactFormat::OnnxFile;
            fs::write(&catalog_path, serde_json::to_vec(&forged).unwrap()).unwrap();
            assert!(VerifiedCatalog::load(&catalog_path, &keys_path).is_err());
        }
        service.remove("speech").unwrap();
        assert_eq!(
            installed_in_process_model(dir.path(), capability).unwrap(),
            None
        );
    }

    fn signed_speech_witness(
        root: &Path,
        release: &LocalModelRelease,
    ) -> (
        crate::catalog::VerifiedCatalog,
        crate::catalog::SignedCatalog,
        PathBuf,
        PathBuf,
    ) {
        use crate::catalog::{
            CATALOG_SIGNATURE_DOMAIN, CatalogPayload, SignedCatalog, VerifiedCatalog,
            catalog_signing_bytes,
        };
        use ed25519_dalek::{Signer as _, SigningKey};
        let key = SigningKey::from_bytes(&[5; 32]);
        let payload = CatalogPayload {
            schema: 1,
            models: vec![release.clone()],
            runtimes: vec![],
            recommendation: String::new(),
        };
        let signed = SignedCatalog {
            schema: 1,
            signature_domain: CATALOG_SIGNATURE_DOMAIN.into(),
            key_id: "isolated-test-only".into(),
            signature: key
                .sign(&catalog_signing_bytes(&payload).unwrap())
                .to_bytes()
                .to_vec(),
            payload,
        };
        let catalog_path = root.join("test-catalog.json");
        let keys_path = root.join("test-keys.json");
        fs::write(&catalog_path, serde_json::to_vec(&signed).unwrap()).unwrap();
        fs::write(&keys_path, serde_json::to_vec(&serde_json::json!({"keys": {"isolated-test-only": hex::encode(key.verifying_key().to_bytes())}})).unwrap()).unwrap();
        let verified = VerifiedCatalog::load(&catalog_path, &keys_path).unwrap();
        (verified, signed, catalog_path, keys_path)
    }

    fn assert_speech_witness(
        root: &Path,
        verified: &crate::catalog::VerifiedCatalog,
        format: ModelArtifactFormat,
        bytes: &[u8],
    ) {
        let sealed = if format == ModelArtifactFormat::SileroGgml {
            assert!(matches!(
                crate::speech::speech_model_file(root, verified),
                Err(crate::speech::SpeechError::ModelUnavailable)
            ));
            crate::speech::speech_activity_file(root, verified)
                .unwrap()
                .unwrap()
        } else {
            assert!(
                crate::speech::speech_activity_file(root, verified)
                    .unwrap()
                    .is_none()
            );
            crate::speech::speech_model_file(root, verified).unwrap()
        };
        assert_eq!(sealed.format(), format);
        assert_eq!(read_speech_witness(root, verified, format).unwrap(), bytes);
        #[cfg(feature = "whisper")]
        if format == ModelArtifactFormat::WhistleCact {
            // These authenticated fixture weights deliberately have the wrong
            // pinned size. Both feature configurations must reject them before
            // configuring a session or entering any native model parser.
            let mut input = std::io::empty();
            let mut output = Vec::new();
            assert_eq!(
                crate::speech::worker::serve(root, verified, &mut input, &mut output),
                Err(crate::speech::SpeechError::ModelUnavailable)
            );
            assert!(output.is_empty());
        }
    }

    fn read_speech_witness(
        root: &Path,
        verified: &crate::catalog::VerifiedCatalog,
        format: ModelArtifactFormat,
    ) -> Result<Vec<u8>, crate::speech::SpeechError> {
        use std::io::Read as _;
        let sealed = if format == ModelArtifactFormat::SileroGgml {
            crate::speech::speech_activity_file(root, verified)?
                .ok_or(crate::speech::SpeechError::ModelUnavailable)?
        } else {
            crate::speech::speech_model_file(root, verified)?
        };
        let mut bytes = Vec::new();
        sealed
            .file()
            .try_clone()
            .map_err(|_| crate::speech::SpeechError::ModelReceipt)?
            .read_to_end(&mut bytes)
            .map_err(|_| crate::speech::SpeechError::ModelReceipt)?;
        Ok(bytes)
    }

    /// An in-process model installs through the verified download and the
    /// licence record alone, is found by capability from the store on disk,
    /// is never started, and disappears from the lookup once removed.
    #[test]
    fn in_process_models_install_without_a_runtime_and_are_found_by_capability() {
        let bytes = b"onnx graph";
        let dir = tempfile::tempdir().unwrap();
        let release = onnx_model(bytes);
        let mut service = service_for(dir.path(), bytes);
        assert_eq!(
            installed_in_process_model(dir.path(), InProcessCapability::Segmentation).unwrap(),
            None
        );
        assert!(matches!(
            service.install_in_process_with_progress(&release),
            Err(ServiceError::LicenseNotAccepted(_))
        ));
        assert!(matches!(
            service.install_in_process_with_progress(&model(bytes)),
            Err(ServiceError::NotInProcess(_))
        ));

        service.record_license_acceptance(&release).unwrap();
        service.queue_install(&release).unwrap();
        assert_eq!(
            installed_in_process_model(dir.path(), InProcessCapability::Segmentation).unwrap(),
            None,
            "a queued download is not an installed model"
        );
        assert_eq!(
            service.install_in_process_with_progress(&release).unwrap(),
            LocalModelStatus::Installed
        );
        assert!(service.supervisor.command.is_none());

        let found = installed_in_process_model(dir.path(), InProcessCapability::Segmentation)
            .unwrap()
            .expect("installed segmentation model");
        assert_eq!(found.id, "subject");
        assert_eq!(found.revision, "c".repeat(64));
        assert_eq!(found.sha256, release.artifact.sha256);
        assert_eq!(found.license_id, "Apache-2.0");
        assert_eq!(
            found.path,
            dir.path().join("sha256").join(&release.artifact.sha256)
        );
        assert_eq!(fs::read(&found.path).unwrap(), bytes);

        service.remove("subject").unwrap();
        assert!(!found.path.exists());
        assert_eq!(
            installed_in_process_model(dir.path(), InProcessCapability::Segmentation).unwrap(),
            None
        );
    }

    /// Acceptance is pinned to the exact model release, status persists and removal stops the supervised
    /// service, and the command is argv that never uses a mutable tag.
    #[test]
    fn the_installed_model_is_pinned_supervised_and_launched_by_digest() {
        // Phase: acceptance_is_pinned_to_the_exact_model_release.
        {
            let bytes = b"licensed";
            let dir = tempfile::tempdir().unwrap();
            let mut licensed = model(bytes);
            licensed.requires_acceptance = true;
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes: bytes.to_vec(),
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            assert!(matches!(
                service.queue_install(&licensed),
                Err(ServiceError::LicenseNotAccepted(_))
            ));
            service.record_license_acceptance(&licensed).unwrap();
            service.queue_install(&licensed).unwrap();
            licensed.version = "2".into();
            assert!(matches!(
                service.queue_install(&licensed),
                Err(ServiceError::LicenseNotAccepted(_))
            ));
        }

        // Phase: status_persists_and_remove_stops_the_supervised_service.
        {
            let bytes = b"model";
            let dir = tempfile::tempdir().unwrap();
            let model = model(bytes);
            let mut service = LocalModelService::open(
                dir.path(),
                Transport {
                    bytes: bytes.to_vec(),
                    starts: vec![],
                },
                Supervisor {
                    command: None,
                    stopped: vec![],
                },
            )
            .unwrap();
            service.install_model(&model).unwrap();
            service.state.models.get_mut("tiny").unwrap().process = Some(ProcessHandle {
                id: "active".into(),
                container: None,
            });
            service.persist().unwrap();
            service.remove("tiny").unwrap();
            assert_eq!(service.supervisor.stopped, ["active"]);
            assert!(
                LocalModelService::open(
                    dir.path(),
                    Transport {
                        bytes: vec![],
                        starts: vec![]
                    },
                    Supervisor {
                        command: None,
                        stopped: vec![]
                    }
                )
                .unwrap()
                .state
                .models
                .is_empty()
            );
        }

        // Phase: command_is_argv_and_never_uses_a_mutable_tag.
        {
            let command = VllmCommand {
                runtime_image: "registry.example.invalid/tilo/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                runtime_kind: RuntimeKind::Cpu,
                args: vec![
                    "serve".into(),
                    "/store/sha256/model".into(),
                    "--host".into(),
                    "127.0.0.1".into(),
                ],
                model_path: PathBuf::from("/store/sha256/model"),
                model_container_path: "/models/model.gguf".into(),
                endpoint: LocalEndpoint {
                    url: "http://127.0.0.1:8000/v1".into(),
                    model_id: "tiny".into(),
                },
                runtime_extension_id: "runtime".into(),
            };
            assert!(command.runtime_image.contains("@sha256:"));
            assert_eq!(command.endpoint.url, "http://127.0.0.1:8000/v1");
        }
    }
}
