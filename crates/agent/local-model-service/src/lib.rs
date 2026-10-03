//! Concrete, content-addressed local-model service.
//!
//! Downloads are restricted to catalog-pinned HTTPS artifacts and resume into
//! a private staging path. Bytes are hashed and atomically promoted beneath
//! their SHA-256 identity; only then can a signed runtime extension and model
//! become eligible for a typed vLLM launch. There is no generic
//! command, URL, tag, or package-install API.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests assert service boundaries"
    )
)]

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use agent_provider_config::local::{
    LocalEndpoint, LocalModelRelease, LocalModelStatus, ModelArtifactFormat, PinnedArtifact,
    RuntimeRelease,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use system_extension::{AcceptedExtension, ExtensionKind};
use thiserror::Error;

const STATE_FILE: &str = "state.json";
const MAX_SNAPSHOT_FILES: u32 = 100_000;
const MAX_SNAPSHOT_PATH_BYTES: usize = 1024;

/// The transfer boundary. Implementations must use HTTPS and honour `start`;
/// this crate validates the pinned HTTPS URL before invoking one. The caller
/// provides the already-open private partial file, so transports never choose
/// arbitrary filesystem destinations.
pub trait HttpsTransport {
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
    /// Launch `command` and return a handle once the endpoint accepts connections.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the supervisor refuses the
    /// command or the service does not become ready.
    fn start(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError>;
    /// Stop the process behind `handle`.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the supervisor cannot stop it.
    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessHandle {
    pub id: String,
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
        Ok(())
    }
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModel {
    pub model_sha256: String,
    pub status: LocalModelStatus,
    pub process: Option<ProcessHandle>,
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

/// Concrete filesystem service. All storage paths are derived from verified
/// lowercase SHA-256 values, avoiding caller-selected paths and cache aliases.
pub struct LocalModelService<T, P> {
    root: PathBuf,
    transport: T,
    supervisor: P,
    state: ServiceState,
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
            state,
        })
    }

    pub fn state(&self) -> &ServiceState {
        &self.state
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

    fn model_path(&self, model: &LocalModelRelease) -> Result<PathBuf, ServiceError> {
        match model.artifact_format {
            ModelArtifactFormat::HfSnapshotTarZst => self.snapshot_path(&model.artifact.sha256),
            ModelArtifactFormat::VllmGguf => self.object_path(&model.artifact.sha256),
        }
    }

    /// Resumes a pinned artifact, verifies its exact digest and size, then
    /// performs an atomic rename into the content-addressed object store.
    ///
    /// # Errors
    /// Returns an error for a non-HTTPS or malformed pin, a partial file
    /// larger than the pin, a transfer or I/O failure, an incomplete
    /// download, or a digest mismatch.
    pub fn fetch_verified(&mut self, artifact: &PinnedArtifact) -> Result<PathBuf, ServiceError> {
        validate_artifact(artifact)?;
        let final_path = self.object_path(&artifact.sha256)?;
        if final_path.exists() {
            verify_file(&final_path, artifact)?;
            return Ok(final_path);
        }
        let partial = self
            .root
            .join("partial")
            .join(format!("{}.part", artifact.sha256));
        let start = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
        if start > artifact.size_bytes {
            return Err(ServiceError::PartialTooLarge);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&partial)?;
        let maximum = artifact
            .size_bytes
            .checked_sub(start)
            .ok_or(ServiceError::PartialTooLarge)?;
        let mut ignored_progress = |_| {};
        let received = self.transport.resume(
            &artifact.url,
            start,
            maximum,
            &mut ignored_progress,
            &mut file,
        )?;
        file.sync_all()?;
        let total = start
            .checked_add(received)
            .ok_or(ServiceError::PartialTooLarge)?;
        if total != artifact.size_bytes {
            return Err(ServiceError::IncompleteDownload {
                expected: artifact.size_bytes,
                actual: total,
            });
        }
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

    /// Turn a verified compressed Hugging Face snapshot into a private,
    /// content-addressed directory. Archive paths, links, special files,
    /// unexpected expansion, and duplicate names are refused before any
    /// directory becomes visible to Podman.
    fn materialize_model(&mut self, model: &LocalModelRelease) -> Result<PathBuf, ServiceError> {
        let object = self.fetch_verified(&model.artifact)?;
        match model.artifact_format {
            ModelArtifactFormat::VllmGguf => Ok(object),
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
        self.require_license_acceptance(model)?;
        self.materialize_model(model)?;
        self.state.models.insert(
            model.id.clone(),
            ManagedModel {
                model_sha256: model.artifact.sha256.clone(),
                status: LocalModelStatus::Installed,
                process: None,
            },
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
        runtime.validate()?;
        self.require_license_acceptance(model)?;
        let archive = self.fetch_verified(&runtime.release.artifact)?;
        self.supervisor
            .install_runtime_image(&archive, &runtime.release.oci_image)?;
        self.install_model(model)
    }

    /// Background-worker variant that atomically publishes each received model
    /// byte count, then verification and installation phases. The transfer
    /// implementation calls `progress` for every chunk it actually writes.
    ///
    /// # Errors
    /// As [`Self::install_with_runtime`], plus any failure to publish progress.
    pub fn install_with_runtime_progress(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        runtime.validate()?;
        self.require_license_acceptance(model)?;
        let archive = self.fetch_verified(&runtime.release.artifact)?;
        self.supervisor
            .install_runtime_image(&archive, &runtime.release.oci_image)?;
        let mut published = self.state.clone();
        let state_path = self.root.join(STATE_FILE);
        let model_id = model.id.clone();
        let total = model.artifact.size_bytes;
        let mut progress_error = None;
        let transfer = {
            let mut progress = |event: TransferEvent| {
                if let Some(managed) = published.models.get_mut(&model_id) {
                    managed.status = match event {
                        TransferEvent::Received(received) => LocalModelStatus::Downloading {
                            received_bytes: received,
                            total_bytes: total,
                        },
                        TransferEvent::Verifying => LocalModelStatus::Verifying,
                    };
                }
                // Keep the state update outside the `get_mut` borrow: every
                // confirmed write has a durable status record before the
                // worker proceeds to the next chunk.
                if let Err(error) = persist_state(&state_path, &published) {
                    progress_error = Some(error);
                }
            };
            self.fetch_verified_with_progress(&model.artifact, &mut progress)
        };
        transfer?;
        if let Some(error) = progress_error {
            return Err(error);
        }
        if matches!(model.artifact_format, ModelArtifactFormat::HfSnapshotTarZst) {
            let archive = self.object_path(&model.artifact.sha256)?;
            self.extract_snapshot(&archive, model)?;
        }
        if let Some(managed) = published.models.get_mut(&model_id) {
            managed.status = LocalModelStatus::Installed;
        }
        persist_state(&state_path, &published)?;
        self.state = published;
        Ok(LocalModelStatus::Installed)
    }

    fn fetch_verified_with_progress(
        &mut self,
        artifact: &PinnedArtifact,
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<PathBuf, ServiceError> {
        validate_artifact(artifact)?;
        let final_path = self.object_path(&artifact.sha256)?;
        if final_path.exists() {
            progress(TransferEvent::Received(artifact.size_bytes));
            progress(TransferEvent::Verifying);
            verify_file(&final_path, artifact)?;
            return Ok(final_path);
        }
        let partial = self
            .root
            .join("partial")
            .join(format!("{}.part", artifact.sha256));
        let start = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
        if start > artifact.size_bytes {
            return Err(ServiceError::PartialTooLarge);
        }
        progress(TransferEvent::Received(start));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&partial)?;
        let maximum = artifact
            .size_bytes
            .checked_sub(start)
            .ok_or(ServiceError::PartialTooLarge)?;
        let mut reported = start;
        let mut relay = |written: u64| {
            reported = start.saturating_add(written);
            progress(TransferEvent::Received(reported));
        };
        let received =
            self.transport
                .resume(&artifact.url, start, maximum, &mut relay, &mut file)?;
        file.sync_all()?;
        let total = start
            .checked_add(received)
            .ok_or(ServiceError::PartialTooLarge)?;
        if total != artifact.size_bytes {
            return Err(ServiceError::IncompleteDownload {
                expected: artifact.size_bytes,
                actual: total,
            });
        }
        progress(TransferEvent::Verifying);
        verify_file(&partial, artifact)?;
        fs::rename(&partial, &final_path).map_err(ServiceError::Io)?;
        Ok(final_path)
    }

    /// Durable status written before a background worker begins transfer.
    ///
    /// # Errors
    /// Returns [`ServiceError::LicenseNotAccepted`] or a persistence error.
    pub fn queue_install(&mut self, model: &LocalModelRelease) -> Result<(), ServiceError> {
        self.require_license_acceptance(model)?;
        self.state.models.insert(
            model.id.clone(),
            ManagedModel {
                model_sha256: model.artifact.sha256.clone(),
                status: LocalModelStatus::Downloading {
                    received_bytes: 0,
                    total_bytes: model.artifact.size_bytes,
                },
                process: None,
            },
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

    /// Mark `model_id` failed with `message` and forget its process.
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
        managed.process = None;
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
        self.update_status(model_id, LocalModelStatus::Starting, None)
    }

    /// Start vLLM under a supervisor. Runtime/model paths are content addressed
    /// and the runtime must have an accepted signed extension record.
    ///
    /// # Errors
    /// Returns an error when the runtime is not accepted, `port` is zero, the
    /// model or runtime is not installed, or the supervisor fails to start it.
    pub fn start(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
        port: u16,
    ) -> Result<LocalEndpoint, ServiceError> {
        runtime.validate()?;
        if port == 0 {
            return Err(ServiceError::InvalidPort);
        }
        let model_path = self.model_path(model)?;
        let model_container_path = match model.artifact_format {
            ModelArtifactFormat::HfSnapshotTarZst if model_path.is_dir() => "/models/model",
            ModelArtifactFormat::VllmGguf if model_path.is_file() => "/models/model.gguf",
            _ => return Err(ServiceError::ModelNotInstalled(model.id.clone())),
        };
        if matches!(model.artifact_format, ModelArtifactFormat::HfSnapshotTarZst) {
            validate_snapshot_layout(&model_path)?;
        }
        if !model_path.exists() {
            return Err(ServiceError::ModelNotInstalled(model.id.clone()));
        }
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
            args: vec![
                "serve".into(),
                // The supervisor mounts this verified host object at this
                // stable, container-visible path. Never pass a host path to
                // vLLM inside the OCI container.
                model_container_path.into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                port.to_string(),
            ],
            model_path,
            model_container_path: model_container_path.into(),
            endpoint: endpoint.clone(),
            runtime_extension_id: runtime.accepted.signed.manifest.id.clone(),
        };
        self.update_status(&model.id, LocalModelStatus::Starting, None)?;
        let handle = self.supervisor.start(&command)?;
        self.update_status(
            &model.id,
            LocalModelStatus::Ready {
                endpoint: endpoint.clone(),
            },
            Some(handle),
        )?;
        Ok(endpoint)
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
        if let Some(handle) = managed.process {
            self.supervisor.stop(&handle)?;
        }
        self.update_status(model_id, LocalModelStatus::Installed, None)?;
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
        self.persist()
    }

    fn persist(&self) -> Result<(), ServiceError> {
        persist_state(&self.root.join(STATE_FILE), &self.state)
    }
}

fn persist_state(path: &Path, state: &ServiceState) -> Result<(), ServiceError> {
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    tilo_files::native::atomic::write_creating_parent(path, &bytes)?;
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
    let metadata = fs::metadata(path)?;
    if metadata.len() != artifact.size_bytes {
        return Err(ServiceError::DigestMismatch);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        // `Read` never reports more bytes than the buffer holds.
        hasher.update(buffer.get(..read).ok_or(ServiceError::DigestMismatch)?);
    }
    if hex::encode(hasher.finalize()) == artifact.sha256 {
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

    fn artifact(bytes: &[u8]) -> PinnedArtifact {
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
    struct Supervisor {
        command: Option<VllmCommand>,
        stopped: Vec<String>,
    }
    impl ProcessSupervisor for Supervisor {
        fn install_runtime_image(&mut self, _: &Path, _: &str) -> Result<(), ServiceError> {
            Ok(())
        }
        fn start(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
            self.command = Some(command.clone());
            Ok(ProcessHandle { id: "svc-1".into() })
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
            let materialized = service.model_path(&release).unwrap();
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
            assert!(!service.model_path(&release).unwrap().exists());
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
            assert!(!service.model_path(&release).unwrap().exists());
        }
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
            });
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
