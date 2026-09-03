//! Concrete, content-addressed local-model service.
//!
//! Downloads are restricted to catalog-pinned HTTPS artifacts and resume into
//! a private staging path. Bytes are hashed and atomically promoted beneath
//! their SHA-256 identity; only then can a signed runtime extension and model
//! become eligible for a typed vLLM launch. There is intentionally no generic
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
use std::path::{Path, PathBuf};

use agent_provider_config::local::{
    LocalEndpoint, LocalModelRelease, LocalModelStatus, PinnedArtifact, RuntimeRelease,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use system_extension::{AcceptedExtension, ExtensionKind};
use thiserror::Error;

const STATE_FILE: &str = "state.json";

/// The transfer boundary. Implementations must use HTTPS and honour `start`;
/// this crate validates the pinned HTTPS URL before invoking one. The caller
/// provides the already-open private partial file, so transports never choose
/// arbitrary filesystem destinations.
pub trait HttpsTransport {
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
    fn install_runtime_image(
        &mut self,
        archive: &Path,
        signed_image: &str,
    ) -> Result<(), ServiceError>;
    fn start(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError>;
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
    pub fn open(
        root: impl Into<PathBuf>,
        transport: T,
        supervisor: P,
    ) -> Result<Self, ServiceError> {
        let root = root.into();
        fs::create_dir_all(root.join("sha256"))?;
        fs::create_dir_all(root.join("partial"))?;
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

    pub fn object_path(&self, sha256: &str) -> Result<PathBuf, ServiceError> {
        validate_sha256(sha256)?;
        Ok(self.root.join("sha256").join(sha256))
    }

    /// Resumes a pinned artifact, verifies its exact digest and size, then
    /// performs an atomic rename into the content-addressed object store.
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
        let start = fs::metadata(&partial)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
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

    /// Record a catalog-verified model as available after placing its bytes.
    pub fn install_model(
        &mut self,
        model: &LocalModelRelease,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.require_license_acceptance(model)?;
        self.fetch_verified(&model.artifact)?;
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
        let start = fs::metadata(&partial)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
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
    /// release. This is intentionally separate from UI state and survives a
    /// transient worker restart, while a changed catalog release must be
    /// confirmed again.
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

    /// Start vLLM under a supervisor. Runtime/model paths are content addressed
    /// and the runtime must have an accepted signed extension record.
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
        let model_path = self.object_path(&model.artifact.sha256)?;
        if !model_path.is_file() {
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
                "/models/model".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                port.to_string(),
            ],
            model_path,
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
    atomic_file::write_creating_parent(path, &bytes)?;
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
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hex::encode(hasher.finalize()) == artifact.sha256 {
        Ok(())
    } else {
        Err(ServiceError::DigestMismatch)
    }
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
    use std::io::Write as _;

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
            license_id: "MIT".into(),
            license_url: "https://catalog.example.invalid/license".into(),
            requires_acceptance: false,
            installed_size_bytes: bytes.len() as u64,
            allowed_runtimes: [RuntimeKind::Cpu].into(),
            required_memory_mib: 1,
            required_accelerator_mib: 0,
            context_window: 1,
        }
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
            let tail = &self.bytes[start as usize..];
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
            let tail = &self.bytes[start as usize..];
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

    #[test]
    fn content_addressed_fetch_resumes_and_promotes_atomically() {
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

    #[test]
    fn verified_fetch_publishes_each_confirmed_byte_count() {
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

    #[test]
    fn acceptance_is_pinned_to_the_exact_model_release() {
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

    #[test]
    fn status_persists_and_remove_stops_the_supervised_service() {
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

    #[test]
    fn command_is_argv_and_never_uses_a_mutable_tag() {
        let command = VllmCommand {
            runtime_image: "registry.example.invalid/qos/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            runtime_kind: RuntimeKind::Cpu,
            args: vec![
                "serve".into(),
                "/store/sha256/model".into(),
                "--host".into(),
                "127.0.0.1".into(),
            ],
            model_path: PathBuf::from("/store/sha256/model"),
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
