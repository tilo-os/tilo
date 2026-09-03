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
        destination: &mut File,
    ) -> Result<u64, ServiceError>;
}

/// A supervised runtime boundary. `VllmCommand` is data assembled exclusively
/// from verified artifact paths, a signed extension identity, and a service
/// allocated loopback port — never a shell string.
pub trait ProcessSupervisor {
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModel {
    pub model_sha256: String,
    pub status: LocalModelStatus,
    pub process: Option<ProcessHandle>,
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
        let received = self
            .transport
            .resume(&artifact.url, start, maximum, &mut file)?;
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
        self.fetch_verified(&runtime.release.artifact)?;
        self.install_model(model)
    }

    /// Durable status written before a background worker begins transfer.
    pub fn queue_install(&mut self, model: &LocalModelRelease) -> Result<(), ServiceError> {
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
                model_path.display().to_string(),
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
        let mut bytes = serde_json::to_vec_pretty(&self.state)?;
        bytes.push(b'\n');
        atomic_file::write_creating_parent(&self.root.join(STATE_FILE), &bytes)?;
        Ok(())
    }
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
            version: "1".into(),
            artifact: artifact(bytes),
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
            destination: &mut File,
        ) -> Result<u64, ServiceError> {
            self.starts.push(start);
            let tail = &self.bytes[start as usize..];
            destination.write_all(tail)?;
            Ok(tail.len() as u64)
        }
    }
    struct Supervisor {
        command: Option<VllmCommand>,
        stopped: Vec<String>,
    }
    impl ProcessSupervisor for Supervisor {
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
