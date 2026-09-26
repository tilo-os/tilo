//! Local model provisioning with no ambient shell access.
//!
//! The desktop decides what package/runtime it trusts through a pinned catalog;
//! this module decides whether it fits the machine and drives only a typed
//! [`LocalModelBackend`]. The backend may use a downloader, OCI manager, and
//! service manager, but this layer never constructs a command line from model
//! metadata or user input.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One accelerator the trusted hardware probe observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accelerator {
    Nvidia { vram_mib: u64, driver: String },
    Amd { vram_mib: u64, rocm: Option<String> },
}

/// The capability snapshot a privileged hardware service publishes. A UI or
/// agent never infers this from a user supplied PCI string.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HardwareSnapshot {
    pub cpu_threads: u16,
    pub memory_mib: u64,
    pub free_storage_mib: u64,
    pub accelerators: Vec<Accelerator>,
}

/// A runtime build selected by trusted hardware facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Cpu,
    NvidiaCuda,
    AmdRocm,
}

impl RuntimeKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::NvidiaCuda => "NVIDIA CUDA",
            Self::AmdRocm => "AMD ROCm",
        }
    }
}

/// A pinned runtime image or package. `artifact` is checksum-addressed, so a
/// mutable tag can never be installed by this planner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRelease {
    pub id: String,
    pub kind: RuntimeKind,
    pub version: String,
    pub artifact: PinnedArtifact,
    /// Immutable OCI image digest prepared by the signed runtime extension.
    /// A runtime is never treated as an executable downloaded blob.
    pub oci_image: String,
}

/// An immutable, verified downloadable object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedArtifact {
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// The only model artifact layouts the local runtime understands. The format
/// is signed catalog data, never guessed from a filename or HTTP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelArtifactFormat {
    /// A complete Hugging Face/vLLM snapshot packaged as a `.tar.zst` stream.
    /// The service verifies the compressed object, then refuses unsafe archive
    /// entries before atomically promoting the extracted directory.
    HfSnapshotTarZst,
    /// An explicit GGUF single-file release. Catalog authors must assert vLLM
    /// compatibility; the format is not inferred as a fallback.
    VllmGguf,
}

impl PinnedArtifact {
    /// Validate the catalog metadata for this immutable artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact is not HTTPS-addressed, lacks a
    /// SHA-256 digest, or has no declared size.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        if !self.url.starts_with("https://") {
            return Err(ProvisionError::InvalidCatalog(
                "artifact URL must use HTTPS",
            ));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ProvisionError::InvalidCatalog(
                "artifact SHA-256 must be 64 hex characters",
            ));
        }
        if self.size_bytes == 0 {
            return Err(ProvisionError::InvalidCatalog(
                "artifact size must be non-zero",
            ));
        }
        Ok(())
    }
}

/// A model release built for a fixed runtime family and quantization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelRelease {
    pub id: String,
    pub label: String,
    /// Release-authored description shown before any bytes are downloaded.
    pub description: String,
    pub version: String,
    pub artifact: PinnedArtifact,
    pub artifact_format: ModelArtifactFormat,
    /// SPDX license identifier where one exists (`Apache-2.0`, `MIT`, ...).
    pub license_id: String,
    /// Immutable model-card or license document for the pinned revision.
    pub license_url: String,
    /// Whether setup must capture explicit license acceptance.
    pub requires_acceptance: bool,
    /// Expected on-disk footprint after unpacking/conversion.
    pub installed_size_bytes: u64,
    pub allowed_runtimes: BTreeSet<RuntimeKind>,
    pub required_memory_mib: u64,
    pub required_accelerator_mib: u64,
    pub context_window: u32,
    /// Signed statement that this exact model revision accepts image input.
    /// The model manager propagates it into provider metadata; no app guesses
    /// this based on a model id or a manual label.
    #[serde(default)]
    pub vision: bool,
    /// Signed declaration for the dedicated local Photos face protocol.
    /// Generic vision labels/descriptions never imply this capability.
    #[serde(default)]
    pub face_recognition: bool,
}

/// Catalogs are supplied by a signed/pinned desktop update channel. They are
/// data, not arbitrary package recipes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalModelCatalog {
    pub runtimes: Vec<RuntimeRelease>,
    pub models: Vec<LocalModelRelease>,
}

impl LocalModelCatalog {
    /// Validate the runtime and model entries supplied by the signed catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry is incomplete, not pinned, or conflicts
    /// with another catalog entry.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        let mut runtime_ids = BTreeSet::new();
        for runtime in &self.runtimes {
            if runtime.id.trim().is_empty() || !runtime_ids.insert(runtime.id.as_str()) {
                return Err(ProvisionError::InvalidCatalog("runtime ids must be unique"));
            }
            runtime.artifact.validate()?;
            if !runtime.oci_image.contains("@sha256:") {
                return Err(ProvisionError::InvalidCatalog(
                    "runtime OCI image must be digest pinned",
                ));
            }
        }
        let mut model_ids = BTreeSet::new();
        for model in &self.models {
            if model.id.trim().is_empty() || !model_ids.insert(model.id.as_str()) {
                return Err(ProvisionError::InvalidCatalog("model ids must be unique"));
            }
            if model.allowed_runtimes.is_empty() {
                return Err(ProvisionError::InvalidCatalog("model supports no runtime"));
            }
            model.artifact.validate()?;
            if model.description.trim().is_empty()
                || model.license_id.trim().is_empty()
                || !model.license_url.starts_with("https://")
            {
                return Err(ProvisionError::InvalidCatalog(
                    "model release metadata must include description, license id, and HTTPS license URL",
                ));
            }
            if model.installed_size_bytes < model.artifact.size_bytes {
                return Err(ProvisionError::InvalidCatalog(
                    "installed model size cannot be smaller than its artifact",
                ));
            }
        }
        Ok(())
    }

    pub fn runtime(&self, kind: RuntimeKind) -> Option<&RuntimeRelease> {
        self.runtimes
            .iter()
            .filter(|runtime| runtime.kind == kind)
            .max_by_key(|runtime| &runtime.version)
    }

    /// Pick the largest known-fitting model for the preferred runtime. The
    /// caller still presents this as a recommendation, never a silent install.
    pub fn recommend<'a>(
        &'a self,
        hardware: &HardwareSnapshot,
        runtime: RuntimeKind,
    ) -> Option<&'a LocalModelRelease> {
        let accelerator = accelerator_memory(hardware, runtime);
        self.models
            .iter()
            .filter(|model| {
                model.allowed_runtimes.contains(&runtime)
                    && model.required_memory_mib <= hardware.memory_mib
                    && model.required_accelerator_mib <= accelerator
                    && model.artifact.size_bytes / (1024 * 1024) <= hardware.free_storage_mib
            })
            .max_by_key(|model| (model.context_window, Reverse(model.artifact.size_bytes)))
    }
}

fn accelerator_memory(hardware: &HardwareSnapshot, runtime: RuntimeKind) -> u64 {
    hardware
        .accelerators
        .iter()
        .filter_map(|accelerator| match (runtime, accelerator) {
            (RuntimeKind::NvidiaCuda, Accelerator::Nvidia { vram_mib, .. })
            | (
                RuntimeKind::AmdRocm,
                Accelerator::Amd {
                    vram_mib,
                    rocm: Some(_),
                    ..
                },
            ) => Some(*vram_mib),
            (RuntimeKind::Cpu, _) => Some(hardware.memory_mib),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Determine the best runtime without probing hardware from an unprivileged
/// agent process. `CUDA` and `ROCm` are selected only when both the driver stack
/// and sufficient VRAM are confirmed; CPU is used otherwise.
pub fn select_runtime(hardware: &HardwareSnapshot) -> RuntimeDecision {
    if hardware.accelerators.iter().any(|accelerator| matches!(accelerator, Accelerator::Nvidia { vram_mib, driver } if *vram_mib >= 8192 && !driver.trim().is_empty())) {
        return RuntimeDecision::Ready(RuntimeKind::NvidiaCuda);
    }
    if hardware.accelerators.iter().any(|accelerator| matches!(accelerator, Accelerator::Amd { vram_mib, rocm: Some(version) } if *vram_mib >= 8192 && !version.trim().is_empty())) {
        return RuntimeDecision::Ready(RuntimeKind::AmdRocm);
    }
    if hardware.memory_mib >= 8192 && hardware.cpu_threads >= 4 {
        RuntimeDecision::Ready(RuntimeKind::Cpu)
    } else {
        RuntimeDecision::Unavailable(
            "Local inference needs at least 8 GiB RAM and four CPU threads, or a supported accelerator.",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeDecision {
    Ready(RuntimeKind),
    Unavailable(&'static str),
}

/// Typed lifecycle state shown to the task manager. It separates
/// downloading from verification and activation: a complete file is not an
/// installed model until the hash was checked, and an installed model is not
/// necessarily running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LocalModelStatus {
    NotInstalled,
    Downloading {
        received_bytes: u64,
        total_bytes: u64,
    },
    Verifying,
    Installed,
    Starting,
    Ready {
        endpoint: LocalEndpoint,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEndpoint {
    pub url: String,
    pub model_id: String,
}

impl LocalEndpoint {
    /// Validate that an activated endpoint is local and identifies a model.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not loopback-only or does not
    /// identify a model.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        let local =
            self.url.starts_with("http://127.0.0.1:") || self.url.starts_with("http://[::1]:");
        if !local {
            return Err(ProvisionError::UnsafeEndpoint);
        }
        if self.model_id.trim().is_empty() {
            return Err(ProvisionError::InvalidCatalog("endpoint model id is empty"));
        }
        Ok(())
    }
}

/// A resumable transfer report. `verified` must stay false until the backend
/// has downloaded the full expected length; hash verification is a distinct,
/// mandatory orchestration step below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferProgress {
    pub received_bytes: u64,
    pub complete: bool,
}

/// The only side-effect boundary for provision/install/start. It has typed
/// operations, pinned artifacts, and a typed vLLM launch request — no `run`,
/// `sh`, or free-form command string exists in this API.
pub trait LocalModelBackend {
    /// Return the number of bytes currently persisted for an artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the partial download cannot be inspected.
    fn partial_size(&self, artifact: &PinnedArtifact) -> Result<u64, ProvisionError>;

    /// Resume downloading an artifact from a previously persisted byte offset.
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer cannot be resumed.
    fn resume_download(
        &mut self,
        artifact: &PinnedArtifact,
        from_byte: u64,
    ) -> Result<TransferProgress, ProvisionError>;
    /// Return the SHA-256 digest of the persisted artifact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the partial download cannot be read or hashed.
    fn sha256_of_partial(&self, artifact: &PinnedArtifact) -> Result<String, ProvisionError>;

    /// Atomically promote a verified artifact to its installed location.
    ///
    /// # Errors
    ///
    /// Returns an error if the verified artifact cannot be promoted.
    fn promote_verified(&mut self, artifact: &PinnedArtifact) -> Result<(), ProvisionError>;

    /// Install a pinned runtime release.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot be installed.
    fn install_runtime(&mut self, runtime: &RuntimeRelease) -> Result<(), ProvisionError>;

    /// Install a verified model release.
    ///
    /// # Errors
    ///
    /// Returns an error if the model cannot be installed.
    fn install_model(&mut self, model: &LocalModelRelease) -> Result<(), ProvisionError>;

    /// Start the model service with the typed, loopback-only launch request.
    ///
    /// # Errors
    ///
    /// Returns an error if the model service cannot be started.
    fn start_vllm(&mut self, launch: VllmLaunch) -> Result<LocalEndpoint, ProvisionError>;
}

/// Sanitised, typed vLLM activation request. The port is loopback-only and is
/// not user-provided; a backend maps this record to its service manager/API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VllmLaunch {
    pub runtime_id: String,
    pub model_id: String,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProvisionError {
    InvalidCatalog(&'static str),
    UnsupportedHardware(String),
    Transfer(String),
    IntegrityMismatch,
    Backend(String),
    UnsafeEndpoint,
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCatalog(reason) => {
                write!(formatter, "invalid local-model catalog: {reason}")
            }
            Self::UnsupportedHardware(reason) | Self::Transfer(reason) | Self::Backend(reason) => {
                formatter.write_str(reason)
            }
            Self::IntegrityMismatch => {
                formatter.write_str("download hash did not match the pinned artifact")
            }
            Self::UnsafeEndpoint => {
                formatter.write_str("local runtime returned a non-loopback endpoint")
            }
        }
    }
}
impl std::error::Error for ProvisionError {}

/// Orchestrates a single catalog-approved runtime+model installation.
pub struct LocalModelProvisioner<'a, B> {
    backend: &'a mut B,
}

impl<'a, B: LocalModelBackend> LocalModelProvisioner<'a, B> {
    pub fn new(backend: &'a mut B) -> Self {
        Self { backend }
    }

    /// Install the selected catalog-approved model and runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog, hardware, transfer, verification, or
    /// backend installation step is invalid or fails.
    pub fn install(
        &mut self,
        catalog: &LocalModelCatalog,
        hardware: &HardwareSnapshot,
        model_id: &str,
    ) -> Result<LocalModelStatus, ProvisionError> {
        catalog.validate()?;
        let runtime = match select_runtime(hardware) {
            RuntimeDecision::Ready(runtime) => runtime,
            RuntimeDecision::Unavailable(reason) => {
                return Err(ProvisionError::UnsupportedHardware(reason.into()));
            }
        };
        let release = catalog
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or(ProvisionError::InvalidCatalog("unknown model id"))?;
        if !release.allowed_runtimes.contains(&runtime) {
            return Err(ProvisionError::UnsupportedHardware(format!(
                "{} cannot run on {}",
                release.label,
                runtime.label()
            )));
        }
        let runtime_release = catalog
            .runtime(runtime)
            .ok_or(ProvisionError::InvalidCatalog(
                "no pinned runtime for selected hardware",
            ))?;
        self.download_verified(&runtime_release.artifact)?;
        self.backend.install_runtime(runtime_release)?;
        self.download_verified(&release.artifact)?;
        self.backend.install_model(release)?;
        Ok(LocalModelStatus::Installed)
    }

    /// Start an installed model and validate its returned loopback endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the hardware, catalog, requested port, backend, or
    /// returned endpoint is invalid.
    pub fn activate(
        &mut self,
        catalog: &LocalModelCatalog,
        hardware: &HardwareSnapshot,
        model_id: &str,
        port: u16,
    ) -> Result<LocalModelStatus, ProvisionError> {
        let runtime = match select_runtime(hardware) {
            RuntimeDecision::Ready(runtime) => runtime,
            RuntimeDecision::Unavailable(reason) => {
                return Err(ProvisionError::UnsupportedHardware(reason.into()));
            }
        };
        let runtime_release = catalog
            .runtime(runtime)
            .ok_or(ProvisionError::InvalidCatalog(
                "no pinned runtime for selected hardware",
            ))?;
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or(ProvisionError::InvalidCatalog("unknown model id"))?;
        if port == 0 {
            return Err(ProvisionError::Backend("port 0 cannot be activated".into()));
        }
        let endpoint = self.backend.start_vllm(VllmLaunch {
            runtime_id: runtime_release.id.clone(),
            model_id: model.id.clone(),
            port,
        })?;
        endpoint.validate()?;
        if endpoint.model_id != model.id {
            return Err(ProvisionError::Backend(
                "runtime reported a different model than requested".into(),
            ));
        }
        Ok(LocalModelStatus::Ready { endpoint })
    }

    fn download_verified(&mut self, artifact: &PinnedArtifact) -> Result<(), ProvisionError> {
        let from = self.backend.partial_size(artifact)?;
        if from > artifact.size_bytes {
            return Err(ProvisionError::Transfer(
                "partial artifact exceeds its pinned size".into(),
            ));
        }
        let transfer = self.backend.resume_download(artifact, from)?;
        if !transfer.complete || transfer.received_bytes != artifact.size_bytes {
            return Err(ProvisionError::Transfer(
                "download did not reach its pinned size".into(),
            ));
        }
        let digest = self.backend.sha256_of_partial(artifact)?;
        if !digest.eq_ignore_ascii_case(&artifact.sha256) {
            return Err(ProvisionError::IntegrityMismatch);
        }
        self.backend.promote_verified(artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(name: &str) -> PinnedArtifact {
        PinnedArtifact {
            url: format!("https://models.example.invalid/{name}"),
            sha256: "a".repeat(64),
            size_bytes: 100,
        }
    }
    fn catalog() -> LocalModelCatalog {
        LocalModelCatalog {
            runtimes: vec![RuntimeRelease {
                id: "vllm-cpu-0.9.0".into(),
                kind: RuntimeKind::Cpu,
                version: "0.9.0".into(),
                artifact: artifact("runtime"),
                oci_image: "registry.example.invalid/tilo/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            }],
            models: vec![LocalModelRelease {
                id: "tiny-q4".into(),
                label: "Tiny Q4".into(),
                description: "A tiny test model".into(),
                version: "1".into(),
                artifact: artifact("model"),
                artifact_format: ModelArtifactFormat::VllmGguf,
                license_id: "Apache-2.0".into(),
                license_url: "https://models.example.invalid/LICENSE".into(),
                requires_acceptance: true,
                installed_size_bytes: 100,
                allowed_runtimes: [RuntimeKind::Cpu].into(),
                required_memory_mib: 8192,
                required_accelerator_mib: 0,
                context_window: 8192,
                vision: false,
                face_recognition: false,
            }],
        }
    }
    fn cpu() -> HardwareSnapshot {
        HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 16384,
            free_storage_mib: 100,
            accelerators: vec![],
        }
    }

    /// The runtime is picked CUDA, then `ROCm`, then CPU only when the facts support it; installation
    /// resumes then verifies before each install; activation refuses a network-facing `vLLM` endpoint.
    #[test]
    fn local_model_setup_follows_the_hardware_and_verifies_before_activating() {
        // Phase: picks_cuda_then_rocm_then_cpu_only_when_facts_support_it.
        {
            assert_eq!(
                select_runtime(&HardwareSnapshot {
                    accelerators: vec![Accelerator::Nvidia {
                        vram_mib: 12288,
                        driver: "560".into()
                    }],
                    ..cpu()
                }),
                RuntimeDecision::Ready(RuntimeKind::NvidiaCuda)
            );
            assert_eq!(
                select_runtime(&HardwareSnapshot {
                    accelerators: vec![Accelerator::Amd {
                        vram_mib: 16384,
                        rocm: Some("6.2".into())
                    }],
                    ..cpu()
                }),
                RuntimeDecision::Ready(RuntimeKind::AmdRocm)
            );
            assert_eq!(
                select_runtime(&cpu()),
                RuntimeDecision::Ready(RuntimeKind::Cpu)
            );
        }

        // Phase: installation_resumes_then_verifies_before_each_install.
        {
            let mut fake = Fake {
                partial: 20,
                calls: vec![],
                endpoint: "http://127.0.0.1:8000/v1".into(),
            };
            let status = LocalModelProvisioner::new(&mut fake)
                .install(&catalog(), &cpu(), "tiny-q4")
                .unwrap();
            assert_eq!(status, LocalModelStatus::Installed);
            assert_eq!(
                fake.calls,
                ["resume", "promote", "runtime", "resume", "promote", "model"]
            );
        }

        // Phase: activation_refuses_a_network_facing_vllm_endpoint.
        {
            let mut fake = Fake {
                partial: 0,
                calls: vec![],
                endpoint: "http://192.168.1.2:8000/v1".into(),
            };
            assert_eq!(
                LocalModelProvisioner::new(&mut fake).activate(&catalog(), &cpu(), "tiny-q4", 8000),
                Err(ProvisionError::UnsafeEndpoint)
            );
        }
    }

    struct Fake {
        partial: u64,
        calls: Vec<&'static str>,
        endpoint: String,
    }
    impl LocalModelBackend for Fake {
        fn partial_size(&self, _: &PinnedArtifact) -> Result<u64, ProvisionError> {
            Ok(self.partial)
        }
        fn resume_download(
            &mut self,
            artifact: &PinnedArtifact,
            from: u64,
        ) -> Result<TransferProgress, ProvisionError> {
            self.calls.push("resume");
            if from != self.partial {
                return Err(ProvisionError::Transfer(
                    "unexpected resume offset".to_string(),
                ));
            }
            Ok(TransferProgress {
                received_bytes: artifact.size_bytes,
                complete: true,
            })
        }
        fn sha256_of_partial(&self, artifact: &PinnedArtifact) -> Result<String, ProvisionError> {
            Ok(artifact.sha256.clone())
        }
        fn promote_verified(&mut self, _: &PinnedArtifact) -> Result<(), ProvisionError> {
            self.calls.push("promote");
            Ok(())
        }
        fn install_runtime(&mut self, _: &RuntimeRelease) -> Result<(), ProvisionError> {
            self.calls.push("runtime");
            Ok(())
        }
        fn install_model(&mut self, _: &LocalModelRelease) -> Result<(), ProvisionError> {
            self.calls.push("model");
            Ok(())
        }
        fn start_vllm(&mut self, _: VllmLaunch) -> Result<LocalEndpoint, ProvisionError> {
            Ok(LocalEndpoint {
                url: self.endpoint.clone(),
                model_id: "tiny-q4".into(),
            })
        }
    }
}
