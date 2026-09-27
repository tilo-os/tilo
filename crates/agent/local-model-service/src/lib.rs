//! Concrete, content-addressed local-model service.
//!
//! Downloads are restricted to catalog-pinned HTTPS artifacts from the
//! vendor's own source and resume into a private staging path. Bytes are
//! hashed and atomically promoted beneath their SHA-256 identity; a snapshot
//! is then assembled from those objects. Runtimes are OCI images pulled from
//! their registry by the signed manifest digest. Only then can a signed
//! runtime extension and model become eligible for a typed launch. There is
//! no generic command, URL, tag, or package-install API.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests assert service boundaries"
    )
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agent_provider_config::local::{
    Accelerator, AcceleratorVendor, Configuration, LocalEndpoint, LocalModelRelease,
    LocalModelStatus, ModelArtifact, PinnedArtifact, Placement, RuntimeEngine, RuntimeKind,
    RuntimeRelease, SnapshotFile,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use system_extension::{AcceptedExtension, ExtensionKind};
use thiserror::Error;

const STATE_FILE: &str = "state.json";
/// Where a runtime container sees its private socket directory.
pub const CONTAINER_SOCKET_DIR: &str = "/run/tilo-model";
/// The socket the runtime serves its API on, inside the socket directory.
pub const SOCKET_NAME: &str = "api.sock";

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

/// A supervised runtime boundary. [`RuntimeCommand`] is data assembled
/// exclusively from verified artifact paths, a signed extension identity and
/// a service-owned socket directory — never a shell string.
pub trait ProcessSupervisor {
    /// Pull the digest-pinned image from its registry and prove that the
    /// local image carries exactly that digest before it becomes launchable.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the pull fails or the local
    /// image does not carry `signed_image`'s digest.
    fn pull_runtime_image(&mut self, signed_image: &str) -> Result<(), ServiceError>;
    /// Launch `command` and return a handle once its API answers.
    ///
    /// # Errors
    /// Returns [`ServiceError::Backend`] when the supervisor refuses the
    /// command or the service does not become ready.
    fn start(&mut self, command: &RuntimeCommand) -> Result<ProcessHandle, ServiceError>;
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

/// The one device a runtime container is given, derived from the probed
/// facts of the configuration it runs; never a caller-supplied path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceAccess {
    /// The CPU alone.
    None,
    /// NVIDIA's container device interface: the card, its CUDA libraries and
    /// its Vulkan driver.
    NvidiaCdi,
    /// The AMD compute and render nodes `ROCm` drives.
    AmdCompute,
    /// One DRM render node (`/dev/dri/renderD128`), which the Mesa Vulkan
    /// drivers in the runtime pack drive.
    RenderNode(PathBuf),
}

/// What the container for `kind` on `device` is given.
///
/// # Errors
/// Returns [`ServiceError::InvalidPlacement`] when the processor and the
/// device do not belong together, or the render node is not a plain
/// `renderD<number>` name.
pub fn device_access(
    kind: RuntimeKind,
    device: Option<&Accelerator>,
) -> Result<DeviceAccess, ServiceError> {
    match (kind, device) {
        (RuntimeKind::Cpu, None) => Ok(DeviceAccess::None),
        (RuntimeKind::NvidiaCuda | RuntimeKind::Vulkan, Some(device))
            if device.vendor == AcceleratorVendor::Nvidia =>
        {
            Ok(DeviceAccess::NvidiaCdi)
        }
        (RuntimeKind::AmdRocm, Some(device)) if device.vendor == AcceleratorVendor::Amd => {
            Ok(DeviceAccess::AmdCompute)
        }
        (RuntimeKind::Vulkan, Some(device)) => {
            let node = device
                .render_node
                .as_deref()
                .filter(|node| {
                    node.strip_prefix("renderD").is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                    })
                })
                .ok_or(ServiceError::InvalidPlacement)?;
            Ok(DeviceAccess::RenderNode(Path::new("/dev/dri").join(node)))
        }
        _ => Err(ServiceError::InvalidPlacement),
    }
}

/// An argv-vector representation suitable for a container supervisor. The
/// service never joins these into a shell command. The container has no
/// network: `socket_dir` is its only channel out, mounted at
/// [`CONTAINER_SOCKET_DIR`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeCommand {
    /// Digest-pinned OCI image from the signed runtime catalog. It is consumed
    /// by the container engine; a downloaded blob is never executed directly.
    pub runtime_image: String,
    pub kind: RuntimeKind,
    pub device: DeviceAccess,
    pub engine: RuntimeEngine,
    pub args: Vec<String>,
    pub model_path: PathBuf,
    /// Fixed container-visible target generated from the signed model format.
    pub model_container_path: String,
    /// Private host directory holding the API socket.
    pub socket_dir: PathBuf,
    pub endpoint: LocalEndpoint,
    pub runtime_extension_id: String,
}

/// The engine arguments for `model` on `kind`, built only from signed catalog
/// fields.
///
/// The model is served under its catalog id, which is what the provider
/// profile and the gateway name in every request; an engine would otherwise
/// answer only to the container path. It listens on a Unix socket in the
/// mounted socket directory, never a TCP port. The context is capped at the
/// signed window so the KV cache is sized for it rather than for the model's
/// maximum, and a signed accelerator share keeps headroom on a card the
/// desktop also uses.
#[must_use]
pub fn runtime_args(
    engine: RuntimeEngine,
    kind: RuntimeKind,
    model: &LocalModelRelease,
    model_container_path: &str,
) -> Vec<String> {
    match engine {
        RuntimeEngine::Vllm => {
            let mut args = vec![
                "serve".into(),
                // The supervisor mounts this verified host object at this
                // stable, container-visible path. Never pass a host path to
                // the engine inside the OCI container.
                model_container_path.into(),
                "--served-model-name".into(),
                model.id.clone(),
                "--max-model-len".into(),
                model.context_window.to_string(),
                "--uds".into(),
                format!("{CONTAINER_SOCKET_DIR}/{SOCKET_NAME}"),
            ];
            if let Some(percent) = model.accelerator_memory_percent {
                args.extend([
                    "--gpu-memory-utilization".into(),
                    format!("{:.2}", f64::from(percent) / 100.0),
                ]);
            }
            args
        }
        // `llama-server` is the image's entrypoint. It binds a Unix socket
        // when `--host` names a path ending in `.sock`. One slot keeps the
        // cache at the signed window; the container holds at most one GPU, so
        // it is `Vulkan0`, and on the CPU nothing is offloaded.
        RuntimeEngine::LlamaCpp => {
            let mut args: Vec<String> = vec![
                "--model".into(),
                model_container_path.into(),
                "--alias".into(),
                model.id.clone(),
                "--ctx-size".into(),
                model.context_window.to_string(),
                "--parallel".into(),
                "1".into(),
                "--host".into(),
                format!("{CONTAINER_SOCKET_DIR}/{SOCKET_NAME}"),
                "--jinja".into(),
                "--no-webui".into(),
                "--offline".into(),
            ];
            let (device, layers) = if kind == RuntimeKind::Cpu {
                ("none", "0")
            } else {
                ("Vulkan0", "all")
            };
            args.extend([
                "--device".into(),
                device.into(),
                "--n-gpu-layers".into(),
                layers.into(),
            ]);
            args
        }
    }
}

/// A desired runtime must have passed `system-extension`'s signed desired-state
/// transaction. The signed extension covers the image manifest; the container
/// engine verifies the same digest on pull, and callers cannot supply an
/// entrypoint or mutable image tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRuntime {
    pub accepted: AcceptedExtension,
    pub release: RuntimeRelease,
}

impl SignedRuntime {
    /// Check that the accepted extension is a local-model runtime and names
    /// exactly this release and its image manifest.
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
            || self.release.image_digest() != Some(self.release.artifact.sha256.as_str())
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
    /// license id and content identity make an old acceptance unusable for a
    /// different release.
    pub accepted_licenses: BTreeMap<String, LicenseAcceptance>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModel {
    /// [`ModelArtifact::identity`] of the installed release.
    pub identity: String,
    /// The configuration it was set up for: the runtime pack, the processor
    /// and the device.
    pub placement: Placement,
    /// SHA-256 of every object the release placed in the store.
    #[serde(default)]
    pub objects: BTreeSet<String>,
    pub status: LocalModelStatus,
    pub process: Option<ProcessHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseAcceptance {
    pub model_version: String,
    pub model_identity: String,
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

fn object_digests(model: &LocalModelRelease) -> BTreeSet<String> {
    model
        .artifact
        .objects()
        .into_iter()
        .map(|artifact| artifact.sha256.clone())
        .collect()
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

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
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

    fn snapshot_path(&self, identity: &str) -> Result<PathBuf, ServiceError> {
        validate_sha256(identity)?;
        Ok(self.root.join("snapshots").join(identity))
    }

    fn model_path(&self, model: &LocalModelRelease) -> Result<PathBuf, ServiceError> {
        match &model.artifact {
            ModelArtifact::HfSnapshot { .. } => self.snapshot_path(&model.artifact.identity()),
            ModelArtifact::Gguf(artifact) => self.object_path(&artifact.sha256),
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
        self.fetch_verified_with_progress(artifact, &mut |_| {})
    }

    /// Place every object of `model` and, for a snapshot, assemble its
    /// directory.
    fn materialize_model(
        &mut self,
        model: &LocalModelRelease,
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<PathBuf, ServiceError> {
        let mut completed = 0_u64;
        for artifact in model.artifact.objects() {
            let offset = completed;
            self.fetch_verified_with_progress(artifact, &mut |event| {
                progress(match event {
                    TransferEvent::Received(bytes) => {
                        TransferEvent::Received(offset.saturating_add(bytes))
                    }
                    TransferEvent::Verifying => {
                        TransferEvent::Received(offset.saturating_add(artifact.size_bytes))
                    }
                });
            })?;
            completed = completed.saturating_add(artifact.size_bytes);
        }
        progress(TransferEvent::Verifying);
        match &model.artifact {
            ModelArtifact::Gguf(artifact) => self.object_path(&artifact.sha256),
            ModelArtifact::HfSnapshot { files } => self.assemble_snapshot(files, model),
        }
    }

    /// Link the verified objects of a snapshot into a private,
    /// content-addressed directory. Paths were validated against the signed
    /// catalogue; nothing outside the staging directory is written, and the
    /// directory only becomes visible to the runtime once complete.
    fn assemble_snapshot(
        &self,
        files: &[SnapshotFile],
        model: &LocalModelRelease,
    ) -> Result<PathBuf, ServiceError> {
        let identity = model.artifact.identity();
        let final_path = self.snapshot_path(&identity)?;
        if final_path.exists() {
            validate_snapshot_layout(&final_path)?;
            return Ok(final_path);
        }
        let installed = files.iter().fold(0_u64, |total, file| {
            total.saturating_add(file.artifact.size_bytes)
        });
        if installed > model.installed_size_bytes {
            return Err(ServiceError::SnapshotTooLarge);
        }
        let staging = self
            .root
            .join("snapshot-staging")
            .join(format!("{identity}.assemble"));
        remove_private_directory(&staging)?;
        fs::create_dir(&staging)?;
        set_private_directory_mode(&staging)?;
        let outcome = files.iter().try_for_each(|file| {
            if !agent_provider_config::local::is_snapshot_path(&file.path) {
                return Err(ServiceError::UnsafeSnapshot);
            }
            let destination = staging.join(&file.path);
            let parent = destination.parent().ok_or(ServiceError::UnsafeSnapshot)?;
            fs::create_dir_all(parent)?;
            set_private_directory_mode(parent)?;
            let object = self.object_path(&file.artifact.sha256)?;
            // A hard link shares the verified bytes; the object store and the
            // snapshot live under one root.
            fs::hard_link(&object, &destination)?;
            set_private_file_mode(&destination)
        });
        if let Err(error) = outcome.and_then(|()| validate_snapshot_layout(&staging)) {
            let _ = remove_private_directory(&staging);
            return Err(error);
        }
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

    fn managed(
        model: &LocalModelRelease,
        placement: &Placement,
        status: LocalModelStatus,
    ) -> ManagedModel {
        ManagedModel {
            identity: model.artifact.identity(),
            placement: placement.clone(),
            objects: object_digests(model),
            status,
            process: None,
        }
    }

    /// Record a catalog-verified model as available after placing its bytes.
    ///
    /// # Errors
    /// Returns [`ServiceError::LicenseNotAccepted`] when the release needs a
    /// recorded acceptance, or any fetch, assembly or persistence error.
    pub fn install_model(
        &mut self,
        model: &LocalModelRelease,
        placement: &Placement,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.require_license_acceptance(model)?;
        model
            .artifact
            .validate()
            .map_err(|_| ServiceError::InvalidArtifact)?;
        self.materialize_model(model, &mut |_| {})?;
        self.state.models.insert(
            model.id.clone(),
            Self::managed(model, placement, LocalModelStatus::Installed),
        );
        self.persist()?;
        Ok(LocalModelStatus::Installed)
    }

    /// Install the accepted, pulled runtime before placing the model, so an
    /// `Installed` model never fails Start merely because its runtime was
    /// omitted from the operation.
    ///
    /// # Errors
    /// Returns an error when the runtime is not accepted, the license is not
    /// accepted, or pulling, fetching or installing fails.
    pub fn install_with_runtime(
        &mut self,
        runtime: &SignedRuntime,
        model: &LocalModelRelease,
        placement: &Placement,
    ) -> Result<LocalModelStatus, ServiceError> {
        runtime.validate()?;
        self.require_license_acceptance(model)?;
        self.supervisor
            .pull_runtime_image(&runtime.release.oci_image)?;
        self.install_model(model, placement)
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
        placement: &Placement,
    ) -> Result<LocalModelStatus, ServiceError> {
        runtime.validate()?;
        self.require_license_acceptance(model)?;
        model
            .artifact
            .validate()
            .map_err(|_| ServiceError::InvalidArtifact)?;
        self.supervisor
            .pull_runtime_image(&runtime.release.oci_image)?;
        let mut published = self.state.clone();
        published
            .models
            .entry(model.id.clone())
            .or_insert_with(|| Self::managed(model, placement, LocalModelStatus::NotInstalled));
        let state_path = self.root.join(STATE_FILE);
        let model_id = model.id.clone();
        let total = model.artifact.download_bytes();
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
            self.materialize_model(model, &mut progress)
        };
        transfer?;
        if let Some(error) = progress_error {
            return Err(error);
        }
        published.models.insert(
            model_id,
            Self::managed(model, placement, LocalModelStatus::Installed),
        );
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
        let received = if maximum == 0 {
            0
        } else {
            let mut relay = |written: u64| {
                progress(TransferEvent::Received(start.saturating_add(written)));
            };
            self.transport
                .resume(&artifact.url, start, maximum, &mut relay, &mut file)?
        };
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

    /// Durable status written before a background worker begins transfer.
    ///
    /// # Errors
    /// Returns [`ServiceError::LicenseNotAccepted`] or a persistence error.
    pub fn queue_install(
        &mut self,
        model: &LocalModelRelease,
        placement: &Placement,
    ) -> Result<(), ServiceError> {
        self.require_license_acceptance(model)?;
        self.state.models.insert(
            model.id.clone(),
            Self::managed(
                model,
                placement,
                LocalModelStatus::Downloading {
                    received_bytes: 0,
                    total_bytes: model.artifact.download_bytes(),
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
                model_identity: model.artifact.identity(),
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
            && accepted.model_identity == model.artifact.identity()
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

    /// Start `configuration` under a supervisor, serving on a socket in
    /// `socket_dir`. Model paths are content addressed, the runtime must have
    /// an accepted signed extension record, and the container is given only
    /// the configuration's own device.
    ///
    /// # Errors
    /// Returns an error when the runtime is not accepted or is not the
    /// configuration's, the placement is not the one the model was set up
    /// for, `socket_dir` is not absolute, the model is not installed, or the
    /// supervisor fails to start it.
    pub fn start(
        &mut self,
        runtime: &SignedRuntime,
        configuration: &Configuration<'_>,
        socket_dir: &Path,
    ) -> Result<LocalEndpoint, ServiceError> {
        runtime.validate()?;
        let model = configuration.model;
        if &runtime.release != configuration.runtime
            || !runtime.release.kinds.contains(&configuration.kind)
            || !model.engines.contains(&runtime.release.engine)
            || !model.allowed_runtimes.contains(&configuration.kind)
        {
            return Err(ServiceError::InvalidPlacement);
        }
        let placed = self
            .state
            .models
            .get(&model.id)
            .ok_or_else(|| ServiceError::ModelNotInstalled(model.id.clone()))?;
        if placed.placement != configuration.placement() {
            return Err(ServiceError::InvalidPlacement);
        }
        let device = device_access(configuration.kind, configuration.device)?;
        let model_path = self.model_path(model)?;
        let model_container_path = match &model.artifact {
            ModelArtifact::HfSnapshot { .. } if model_path.is_dir() => {
                validate_snapshot_layout(&model_path)?;
                "/models/model"
            }
            ModelArtifact::Gguf(_) if model_path.is_file() => "/models/model.gguf",
            _ => return Err(ServiceError::ModelNotInstalled(model.id.clone())),
        };
        let endpoint = LocalEndpoint {
            socket: socket_dir.join(SOCKET_NAME),
            model_id: model.id.clone(),
        };
        endpoint
            .validate()
            .map_err(|_| ServiceError::InvalidEndpoint)?;
        let command = RuntimeCommand {
            runtime_image: runtime.release.oci_image.clone(),
            kind: configuration.kind,
            device,
            engine: runtime.release.engine,
            args: runtime_args(
                runtime.release.engine,
                configuration.kind,
                model,
                model_container_path,
            ),
            model_path,
            model_container_path: model_container_path.into(),
            socket_dir: socket_dir.to_path_buf(),
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

    /// Stop first, then delete the model's snapshot and every object no other
    /// installed model also holds. Runtime images are never inferred from a
    /// model removal.
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
        let snapshot = self.snapshot_path(&managed.identity)?;
        if snapshot.exists() {
            remove_private_directory(&snapshot)?;
        }
        let shared: BTreeSet<&String> = self
            .state
            .models
            .iter()
            .filter(|(id, _)| id.as_str() != model_id)
            .flat_map(|(_, other)| &other.objects)
            .collect();
        for object in managed
            .objects
            .iter()
            .filter(|object| !shared.contains(object))
        {
            let path = self.object_path(object)?;
            if path.exists() {
                fs::remove_file(path)?;
            }
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

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), ServiceError> {
    // The user service and rootless Podman share this UID. Keep the snapshot
    // private to it while retaining owner write/search permission during
    // assembly and future atomic cleanup.
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
    #[error("unsafe model snapshot")]
    UnsafeSnapshot,
    #[error("model snapshot is larger than its signed installed size")]
    SnapshotTooLarge,
    #[error("model snapshot is missing config, tokenizer, or weights")]
    InvalidSnapshotLayout,
    #[error("runtime is not an accepted signed local-model extension")]
    RuntimeNotAccepted,
    #[error("this configuration does not run here")]
    InvalidPlacement,
    #[error("model is not installed: {0}")]
    ModelNotInstalled(String),
    #[error("model license has not been accepted for this exact release: {0}")]
    LicenseNotAccepted(String),
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
    use agent_provider_config::local::{ExpectedSpeed, Fit, SpeedClass};
    use std::io::Write as _;

    fn artifact(bytes: &[u8]) -> PinnedArtifact {
        PinnedArtifact {
            url: "https://catalog.example.invalid/object".into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
        }
    }
    /// A single-file GGUF release llama.cpp runs through Vulkan or on the
    /// CPU.
    fn model(bytes: &[u8]) -> LocalModelRelease {
        LocalModelRelease {
            id: "tiny".into(),
            label: "Tiny".into(),
            description: "A tiny test model".into(),
            version: "1".into(),
            artifact: ModelArtifact::Gguf(artifact(bytes)),
            license_id: "MIT".into(),
            license_url: "https://catalog.example.invalid/license".into(),
            requires_acceptance: false,
            installed_size_bytes: bytes.len() as u64,
            engines: [RuntimeEngine::LlamaCpp].into(),
            allowed_runtimes: [RuntimeKind::Vulkan, RuntimeKind::Cpu].into(),
            required_memory_mib: 1,
            required_accelerator_mib: 1,
            context_window: 1,
            capabilities: BTreeSet::new(),
            accelerator_memory_percent: None,
        }
    }

    /// A snapshot release vLLM serves on a CUDA card, over `files`, and a
    /// transport that serves each file's bytes by URL.
    fn snapshot(files: &[(&str, &[u8])]) -> (LocalModelRelease, Objects) {
        let mut release = model(b"unused");
        release.engines = [RuntimeEngine::Vllm].into();
        release.allowed_runtimes = [RuntimeKind::NvidiaCuda].into();
        let mut objects = BTreeMap::new();
        let files = files
            .iter()
            .map(|(path, bytes)| {
                let mut pin = artifact(bytes);
                pin.url = format!(
                    "https://huggingface.co/acme/model/resolve/{}/{path}",
                    "a".repeat(40)
                );
                objects.insert(pin.url.clone(), bytes.to_vec());
                SnapshotFile {
                    path: (*path).into(),
                    artifact: pin,
                }
            })
            .collect::<Vec<_>>();
        release.installed_size_bytes = files.iter().map(|file| file.artifact.size_bytes).sum();
        release.artifact = ModelArtifact::HfSnapshot { files };
        (
            release,
            Objects {
                bytes: objects,
                starts: Vec::new(),
            },
        )
    }

    struct Objects {
        bytes: BTreeMap<String, Vec<u8>>,
        starts: Vec<(String, u64)>,
    }
    impl HttpsTransport for Objects {
        fn resume(
            &mut self,
            url: &str,
            start: u64,
            _: u64,
            progress: &mut dyn FnMut(u64),
            destination: &mut File,
        ) -> Result<u64, ServiceError> {
            self.starts.push((url.into(), start));
            let bytes = self.bytes.get(url).ok_or(ServiceError::InvalidArtifact)?;
            let from = usize::try_from(start).map_err(|_| ServiceError::PartialTooLarge)?;
            let tail = bytes.get(from..).ok_or(ServiceError::PartialTooLarge)?;
            let split = tail.len().min(2);
            destination.write_all(&tail[..split])?;
            progress(split as u64);
            destination.write_all(&tail[split..])?;
            progress(tail.len() as u64);
            Ok(tail.len() as u64)
        }
    }
    fn serving(bytes: &[u8]) -> Objects {
        Objects {
            bytes: [(
                "https://catalog.example.invalid/object".to_string(),
                bytes.to_vec(),
            )]
            .into(),
            starts: Vec::new(),
        }
    }

    /// A fake runtime supervisor: it records what it would run and answers at
    /// once, so a launch is checked without a container engine.
    #[derive(Default)]
    struct Supervisor {
        pulled: Vec<String>,
        command: Option<RuntimeCommand>,
        stopped: Vec<String>,
    }
    impl ProcessSupervisor for Supervisor {
        fn pull_runtime_image(&mut self, image: &str) -> Result<(), ServiceError> {
            self.pulled.push(image.into());
            Ok(())
        }
        fn start(&mut self, command: &RuntimeCommand) -> Result<ProcessHandle, ServiceError> {
            self.command = Some(command.clone());
            Ok(ProcessHandle { id: "svc-1".into() })
        }
        fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
            self.stopped.push(handle.id.clone());
            Ok(())
        }
    }

    fn signed(
        id: &str,
        engine: RuntimeEngine,
        kinds: &[RuntimeKind],
        repository: &str,
    ) -> SignedRuntime {
        let manifest = format!("{{\"schemaVersion\":2,\"id\":\"{id}\"}}");
        let digest = hex::encode(Sha256::digest(manifest.as_bytes()));
        let pin = PinnedArtifact {
            url: format!("https://registry.example.invalid/v2/{id}/manifests/sha256:{digest}"),
            sha256: digest.clone(),
            size_bytes: manifest.len() as u64,
        };
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed = system_extension::SignedExtensionManifest::sign(
            system_extension::ExtensionManifest {
                schema_version: system_extension::SCHEMA_VERSION,
                id: id.into(),
                kind: ExtensionKind::LocalModelRuntime,
                version: "1".into(),
                artifact: system_extension::PinnedArtifact {
                    source: pin.url.clone(),
                    sha256: pin.sha256.clone(),
                    size_bytes: pin.size_bytes,
                },
                target: system_extension::ImageConstraint {
                    base_image_digest: format!("sha256:{}", "c".repeat(64)),
                    architecture: None,
                    kernel_abi: None,
                },
                requires_reboot: false,
            },
            "test",
            &signing,
        )
        .unwrap();
        SignedRuntime {
            accepted: AcceptedExtension {
                signed,
                artifact: system_extension::ArtifactReceipt {
                    sha256: pin.sha256.clone(),
                    size_bytes: pin.size_bytes,
                },
                committed_revision: 1,
            },
            release: RuntimeRelease {
                id: id.into(),
                kinds: kinds.iter().copied().collect(),
                version: "1".into(),
                engine,
                artifact: pin,
                oci_image: format!("{repository}@sha256:{digest}"),
            },
        }
    }

    fn runtime() -> SignedRuntime {
        signed(
            "vllm",
            RuntimeEngine::Vllm,
            &[RuntimeKind::NvidiaCuda],
            "docker.io/vllm/vllm-openai",
        )
    }

    fn llama() -> SignedRuntime {
        signed(
            "llama-cpp-vulkan",
            RuntimeEngine::LlamaCpp,
            &[RuntimeKind::Vulkan, RuntimeKind::Cpu],
            "ghcr.io/ggml-org/llama.cpp",
        )
    }

    fn card() -> Accelerator {
        Accelerator {
            vendor: AcceleratorVendor::Nvidia,
            name: "NVIDIA GeForce RTX 4070 Laptop GPU".into(),
            pci_slot: "0000:01:00.0".into(),
            render_node: Some("renderD129".into()),
            kernel_driver: "nvidia".into(),
            integrated: false,
            dedicated_mib: 8188,
            shared_mib: 0,
            compute: Some("615.71.09".into()),
            restart: None,
        }
    }

    fn igpu() -> Accelerator {
        Accelerator {
            vendor: AcceleratorVendor::Amd,
            name: "AMD Radeon 780M Graphics".into(),
            pci_slot: "0000:66:00.0".into(),
            render_node: Some("renderD128".into()),
            kernel_driver: "amdgpu".into(),
            integrated: true,
            dedicated_mib: 512,
            shared_mib: 15_644,
            compute: None,
            restart: None,
        }
    }

    fn configuration<'a>(
        runtime: &'a SignedRuntime,
        model: &'a LocalModelRelease,
        kind: RuntimeKind,
        device: Option<&'a Accelerator>,
    ) -> Configuration<'a> {
        Configuration {
            runtime: &runtime.release,
            model,
            kind,
            device,
            required_mib: 1,
            budget_mib: 2,
            shared_memory: device.is_none_or(|device| device.integrated),
            fit: Fit::Good,
            speed: ExpectedSpeed {
                tokens_per_second: 20,
                prompt_tokens_per_second: 200,
                answer_seconds: 12,
                class: SpeedClass::Interactive,
            },
        }
    }

    fn placed(
        runtime: &SignedRuntime,
        kind: RuntimeKind,
        device: Option<&Accelerator>,
    ) -> Placement {
        Placement {
            runtime: runtime.release.id.clone(),
            kind,
            device: device.map(|device| device.pci_slot.clone()),
        }
    }

    fn on_card() -> Placement {
        placed(&runtime(), RuntimeKind::NvidiaCuda, Some(&card()))
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
            let mut service =
                LocalModelService::open(dir.path(), serving(bytes), Supervisor::default()).unwrap();
            let object = service.fetch_verified(&item).unwrap();
            assert_eq!(fs::read(object).unwrap(), bytes);
            assert_eq!(service.transport.starts[0].1, 3);
            assert!(!partial.exists());
        }

        // Phase: verified_fetch_publishes_each_confirmed_byte_count.
        {
            let bytes = b"chunked";
            let dir = tempfile::tempdir().unwrap();
            let item = artifact(bytes);
            let mut service =
                LocalModelService::open(dir.path(), serving(bytes), Supervisor::default()).unwrap();
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

    /// A snapshot is fetched file by file and assembled from verified objects; a tampered file never
    /// becomes part of it; a snapshot without weights or beyond its signed size is refused.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn model_snapshots_extract_only_within_their_signed_shape() {
        // Phase: a_snapshot_is_fetched_file_by_file_and_assembled_from_verified_objects.
        {
            let (release, objects) = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{\"t\":1}"),
                ("weights/model-00001-of-00001.safetensors", b"weights"),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            service
                .install_with_runtime_progress(&runtime(), &release, &on_card())
                .unwrap();
            assert_eq!(service.supervisor.pulled.len(), 1);
            assert!(service.supervisor.pulled[0].contains("@sha256:"));
            let materialized = service.model_path(&release).unwrap();
            assert!(materialized.is_dir());
            assert_eq!(fs::read(materialized.join("config.json")).unwrap(), b"{}");
            assert_eq!(
                fs::read(materialized.join("weights/model-00001-of-00001.safetensors")).unwrap(),
                b"weights"
            );
            let requested: Vec<&str> = service
                .transport
                .starts
                .iter()
                .map(|(url, _)| url.as_str())
                .collect();
            assert_eq!(requested.len(), 3);
            assert!(
                requested
                    .iter()
                    .all(|url| url.starts_with("https://huggingface.co/"))
            );
            assert_eq!(
                service.state.models["tiny"].status,
                LocalModelStatus::Installed
            );
            assert_eq!(service.state.models["tiny"].placement, on_card());
        }

        // Phase: a_tampered_file_never_becomes_part_of_a_snapshot.
        {
            let (release, mut objects) = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{}"),
                ("model.safetensors", b"weights"),
            ]);
            for bytes in objects.bytes.values_mut() {
                if bytes == b"weights" {
                    *bytes = b"tampered".to_vec();
                }
            }
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            assert!(service.install_model(&release, &on_card()).is_err());
            assert!(!service.model_path(&release).unwrap().exists());
        }

        // Phase: a_snapshot_without_weights_or_beyond_its_signed_size_is_refused.
        {
            let (release, objects) = snapshot(&[("config.json", b"{}"), ("tokenizer.json", b"{}")]);
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            assert!(matches!(
                service.install_model(&release, &on_card()),
                Err(ServiceError::InvalidSnapshotLayout)
            ));
            assert!(!service.model_path(&release).unwrap().exists());

            let (mut release, objects) = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{}"),
                ("model.safetensors", b"weights"),
            ]);
            release.installed_size_bytes = 4;
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            assert!(matches!(
                service.install_model(&release, &on_card()),
                Err(ServiceError::SnapshotTooLarge)
            ));
        }
    }

    /// Acceptance is pinned to the exact model release, removal stops the service and keeps objects
    /// another model holds, a runtime image must name its signed manifest, and vLLM serves the
    /// catalog id only through its socket within the signed context and memory share.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn the_installed_model_is_pinned_supervised_and_launched_by_digest() {
        // Phase: acceptance_is_pinned_to_the_exact_model_release.
        {
            let bytes = b"licensed";
            let dir = tempfile::tempdir().unwrap();
            let mut licensed = model(bytes);
            licensed.requires_acceptance = true;
            let mut service =
                LocalModelService::open(dir.path(), serving(bytes), Supervisor::default()).unwrap();
            let cpu = placed(&llama(), RuntimeKind::Cpu, None);
            assert!(matches!(
                service.queue_install(&licensed, &cpu),
                Err(ServiceError::LicenseNotAccepted(_))
            ));
            service.record_license_acceptance(&licensed).unwrap();
            service.queue_install(&licensed, &cpu).unwrap();
            licensed.version = "2".into();
            assert!(matches!(
                service.queue_install(&licensed, &cpu),
                Err(ServiceError::LicenseNotAccepted(_))
            ));
        }

        // Phase: remove_stops_the_service_and_keeps_objects_another_model_holds.
        {
            let (release, objects) = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{\"tok\":1}"),
                ("model.safetensors", b"weights"),
            ]);
            let mut sibling = release.clone();
            sibling.id = "sibling".into();
            if let ModelArtifact::HfSnapshot { files } = &mut sibling.artifact {
                files.retain(|file| file.path != "tokenizer.json");
                files.push(SnapshotFile {
                    path: "tokenizer_config.json".into(),
                    artifact: files[0].artifact.clone(),
                });
            }
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            service.install_model(&release, &on_card()).unwrap();
            service.install_model(&sibling, &on_card()).unwrap();
            service.state.models.get_mut("tiny").unwrap().process = Some(ProcessHandle {
                id: "active".into(),
            });
            service.remove("tiny").unwrap();
            assert_eq!(service.supervisor.stopped, ["active"]);
            assert!(!service.model_path(&release).unwrap().exists());
            assert!(service.model_path(&sibling).unwrap().is_dir());
            let ModelArtifact::HfSnapshot { files } = &release.artifact else {
                panic!("snapshot fixture");
            };
            let object = |path: &str| {
                let file = files.iter().find(|file| file.path == path).unwrap();
                service.object_path(&file.artifact.sha256).unwrap()
            };
            assert!(
                object("model.safetensors").exists(),
                "still held by the sibling"
            );
            assert!(
                !object("tokenizer.json").exists(),
                "held by the removed model only"
            );
            let reopened =
                LocalModelService::open(dir.path(), serving(b""), Supervisor::default()).unwrap();
            assert_eq!(
                reopened.state.models.keys().collect::<Vec<_>>(),
                ["sibling"]
            );
        }

        // Phase: a_runtime_whose_image_names_another_manifest_is_refused.
        {
            let mut runtime = runtime();
            runtime.release.oci_image =
                format!("docker.io/vllm/vllm-openai@sha256:{}", "d".repeat(64));
            assert!(matches!(
                runtime.validate(),
                Err(ServiceError::RuntimeNotAccepted)
            ));
        }

        // Phase: a_started_model_is_reached_only_through_its_socket.
        {
            let (release, objects) = snapshot(&[
                ("config.json", b"{}"),
                ("tokenizer.json", b"{}"),
                ("model.safetensors", b"weights"),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let mut service =
                LocalModelService::open(dir.path(), objects, Supervisor::default()).unwrap();
            service.install_model(&release, &on_card()).unwrap();
            let sockets = dir.path().join("run");
            let (runtime, card) = (runtime(), card());
            let on_card = configuration(&runtime, &release, RuntimeKind::NvidiaCuda, Some(&card));
            let endpoint = service.start(&runtime, &on_card, &sockets).unwrap();
            assert_eq!(endpoint.socket, sockets.join(SOCKET_NAME));
            let command = service.supervisor.command.clone().unwrap();
            assert_eq!(command.socket_dir, sockets);
            assert_eq!(command.device, DeviceAccess::NvidiaCdi);
            assert!(command.runtime_image.contains("@sha256:"));
            assert!(matches!(
                service.start(&runtime, &on_card, Path::new("relative")),
                Err(ServiceError::InvalidEndpoint)
            ));
        }

        // Phase: vllm_serves_the_catalog_id_on_a_socket_within_the_signed_context_and_memory_share.
        {
            let mut release = model(b"weights");
            release.id = "qwen/qwen3-4b-awq".into();
            release.context_window = 8192;
            let args = runtime_args(
                RuntimeEngine::Vllm,
                RuntimeKind::NvidiaCuda,
                &release,
                "/models/model",
            );
            let after = |flag: &str| {
                args.iter()
                    .position(|arg| arg == flag)
                    .and_then(|index| args.get(index + 1))
                    .map(String::as_str)
            };
            assert_eq!(args.get(1).map(String::as_str), Some("/models/model"));
            assert_eq!(after("--served-model-name"), Some("qwen/qwen3-4b-awq"));
            assert_eq!(after("--max-model-len"), Some("8192"));
            assert_eq!(after("--uds"), Some("/run/tilo-model/api.sock"));
            assert!(!args.iter().any(|arg| arg == "--host" || arg == "--port"));
            assert_eq!(after("--gpu-memory-utilization"), None);
            release.accelerator_memory_percent = Some(70);
            let args = runtime_args(
                RuntimeEngine::Vllm,
                RuntimeKind::NvidiaCuda,
                &release,
                "/models/model",
            );
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--gpu-memory-utilization", "0.70"])
            );
        }
    }

    /// llama-server runs the GGUF under its catalogue id, on the socket only, within the signed
    /// window; the container gets the configuration's own render node and nothing else; a
    /// placement the model was not set up for, or a device that does not belong to it, is refused.
    #[test]
    fn llama_server_runs_the_gguf_on_its_own_device_and_socket_only() {
        let bytes = b"GGUF weights";
        let mut release = model(bytes);
        release.id = "qwen/qwen3-4b-gguf".into();
        release.context_window = 8192;
        let dir = tempfile::tempdir().unwrap();
        let mut service =
            LocalModelService::open(dir.path(), serving(bytes), Supervisor::default()).unwrap();
        let (runtime, igpu, card) = (llama(), igpu(), card());
        service
            .install_with_runtime_progress(
                &runtime,
                &release,
                &placed(&runtime, RuntimeKind::Vulkan, Some(&igpu)),
            )
            .unwrap();
        assert_eq!(
            service.supervisor.pulled.as_slice(),
            std::slice::from_ref(&runtime.release.oci_image)
        );
        let sockets = dir.path().join("run");
        let on_igpu = configuration(&runtime, &release, RuntimeKind::Vulkan, Some(&igpu));
        let endpoint = service.start(&runtime, &on_igpu, &sockets).unwrap();
        assert_eq!(endpoint.model_id, "qwen/qwen3-4b-gguf");
        let command = service.supervisor.command.clone().unwrap();
        assert_eq!(command.engine, RuntimeEngine::LlamaCpp);
        assert_eq!(
            command.device,
            DeviceAccess::RenderNode("/dev/dri/renderD128".into())
        );
        assert_eq!(command.model_container_path, "/models/model.gguf");
        let after = |flag: &str| {
            command
                .args
                .iter()
                .position(|arg| arg == flag)
                .and_then(|index| command.args.get(index + 1))
                .map(String::as_str)
        };
        assert_eq!(after("--model"), Some("/models/model.gguf"));
        assert_eq!(after("--alias"), Some("qwen/qwen3-4b-gguf"));
        assert_eq!(after("--ctx-size"), Some("8192"));
        assert_eq!(after("--host"), Some("/run/tilo-model/api.sock"));
        assert_eq!(after("--device"), Some("Vulkan0"));
        assert_eq!(after("--n-gpu-layers"), Some("all"));
        assert!(!command.args.iter().any(|arg| arg == "--port"));
        assert!(command.args.iter().any(|arg| arg == "--offline"));

        // On the CPU nothing is offloaded.
        let cpu = runtime_args(
            RuntimeEngine::LlamaCpp,
            RuntimeKind::Cpu,
            &release,
            "/models/model.gguf",
        );
        assert!(cpu.windows(2).any(|pair| pair == ["--device", "none"]));
        assert!(cpu.windows(2).any(|pair| pair == ["--n-gpu-layers", "0"]));

        // It was set up for the iGPU: the card is another configuration.
        let on_card = configuration(&runtime, &release, RuntimeKind::Vulkan, Some(&card));
        assert!(matches!(
            service.start(&runtime, &on_card, &sockets),
            Err(ServiceError::InvalidPlacement)
        ));
        // vLLM does not load a GGUF, whatever the placement says.
        let vllm = super::tests::runtime();
        let wrong_engine = configuration(&vllm, &release, RuntimeKind::NvidiaCuda, Some(&card));
        assert!(matches!(
            service.start(&vllm, &wrong_engine, &sockets),
            Err(ServiceError::InvalidPlacement)
        ));
        // A render node is a plain `renderD<number>` name, never a path.
        let mut odd = igpu.clone();
        odd.render_node = Some("../../dev/mem".into());
        assert!(matches!(
            device_access(RuntimeKind::Vulkan, Some(&odd)),
            Err(ServiceError::InvalidPlacement)
        ));
        assert!(matches!(
            device_access(RuntimeKind::Cpu, Some(&igpu)),
            Err(ServiceError::InvalidPlacement)
        ));
        assert_eq!(
            device_access(RuntimeKind::Vulkan, Some(&card)).unwrap(),
            DeviceAccess::NvidiaCdi
        );
    }
}
