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
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

pub mod snapshot;

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
    /// Signed engine and bounded launch tuning. Omission preserves legacy
    /// vLLM catalog signing bytes exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<RuntimeExecution>,
}

/// Engine-specific settings. No caller-supplied command or arbitrary argv.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeExecution {
    Vllm {
        max_context_tokens: u32,
        cpu_offload_gib: u16,
    },
    LlamaCpp {
        max_context_tokens: u32,
        gpu_layers: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parallel_slots: Option<NonZeroU16>,
    },
}

impl RuntimeRelease {
    /// Whether this signed engine accepts the explicitly declared layout.
    pub const fn supports_format(&self, format: ModelArtifactFormat) -> bool {
        match &self.execution {
            Some(RuntimeExecution::LlamaCpp { .. }) => {
                matches!(format, ModelArtifactFormat::LlamaCppGguf)
            }
            None | Some(RuntimeExecution::Vllm { .. }) => matches!(
                format,
                ModelArtifactFormat::HfSnapshotTarZst | ModelArtifactFormat::VllmGguf
            ),
        }
    }

    /// Validate finite execution limits before installation or launch.
    /// # Errors
    /// Rejects zero/excessive context and impossible CPU-only GPU offload.
    pub fn validate_execution(&self) -> Result<(), ProvisionError> {
        let Some(execution) = &self.execution else {
            return Ok(());
        };
        let context = match execution {
            RuntimeExecution::Vllm {
                max_context_tokens,
                cpu_offload_gib,
            } => {
                if *cpu_offload_gib > 1024 {
                    return Err(ProvisionError::InvalidCatalog(
                        "CPU offload exceeds launch limit",
                    ));
                }
                *max_context_tokens
            }
            RuntimeExecution::LlamaCpp {
                max_context_tokens,
                gpu_layers,
                parallel_slots,
            } => {
                if *gpu_layers > 4096 || (self.kind == RuntimeKind::Cpu && *gpu_layers != 0) {
                    return Err(ProvisionError::InvalidCatalog(
                        "GPU layer setting is incompatible with runtime",
                    ));
                }
                if parallel_slots.is_some_and(|slots| slots.get() > 64) {
                    return Err(ProvisionError::InvalidCatalog(
                        "parallel slots exceed launch limit",
                    ));
                }
                *max_context_tokens
            }
        };
        if context == 0 || context > 1_048_576 {
            return Err(ProvisionError::InvalidCatalog(
                "runtime context is outside launch limits",
            ));
        }
        Ok(())
    }
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
    /// A GGUF release explicitly assigned to llama.cpp server.
    LlamaCppGguf,
    /// One ONNX graph that a per-user service loads into its own process and
    /// runs on the CPU. It needs no signed runtime, container or service
    /// start: the verified content-addressed object is the whole install.
    OnnxFile,
    /// One whisper.cpp GGML speech graph. Its format is the signed declaration
    /// of speech recognition; it is never treated as a chat model or launched
    /// by the vLLM supervisor. The consumer checks its language capabilities.
    WhisperGgml,
    /// Whistle CACT weights, interpreted by the immutable image AOT runtime.
    /// The downloadable object contains weights, never executable runtime code.
    WhistleCact,
    /// CPU Silero GGML acoustic graph; observes speech activity, never text.
    SileroGgml,
}

impl ModelArtifactFormat {
    /// Whether models of this format run inside the process that uses them
    /// rather than behind a supervised vLLM runtime.
    pub const fn runs_in_process(self) -> bool {
        matches!(
            self,
            Self::OnnxFile | Self::WhisperGgml | Self::WhistleCact | Self::SileroGgml
        )
    }

    pub const fn is_speech(self) -> bool {
        matches!(self, Self::WhisperGgml | Self::WhistleCact)
    }
}

/// What an in-process model does. A consumer asks for a capability, never
/// for a model id, so a newer signed release can replace an older one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InProcessCapability {
    /// Salient-object segmentation: one image in, a foreground mask out.
    Segmentation,
    /// On-device speech recognition; the native consumer owns microphone
    /// permission and inference cancellation, not the model installer.
    SpeechRecognition,
    /// Independent acoustic evidence for dictation; never microphone permission.
    SpeechActivity,
}

impl InProcessCapability {
    pub const ALL: [Self; 1] = [Self::Segmentation];
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent signed declaration in the catalog wire format"
)]
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
    /// Signed declaration that this exact release is a salient-object
    /// segmentation model. Only an in-process format may carry it.
    #[serde(default)]
    pub segmentation: bool,
}

impl LocalModelRelease {
    /// Whether this release runs inside the process that uses it.
    pub const fn runs_in_process(&self) -> bool {
        self.artifact_format.runs_in_process()
    }

    /// The in-process capabilities this release declares.
    pub fn in_process_capabilities(&self) -> Vec<InProcessCapability> {
        let mut capabilities = Vec::new();
        if self.segmentation {
            capabilities.push(InProcessCapability::Segmentation);
        }
        if self.artifact_format.is_speech() {
            capabilities.push(InProcessCapability::SpeechRecognition);
        }
        if self.artifact_format == ModelArtifactFormat::SileroGgml {
            capabilities.push(InProcessCapability::SpeechActivity);
        }
        capabilities
    }

    /// Check the format against the capabilities it claims. An in-process
    /// model names no vLLM runtime and no context window, cannot claim chat,
    /// vision or face capabilities, and must say what it does; a runtime
    /// model needs a runtime and a context window and cannot claim an
    /// in-process capability.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first contradiction.
    pub fn validate_shape(&self) -> Result<(), ProvisionError> {
        if self.artifact_format == ModelArtifactFormat::LlamaCppGguf
            && (self.vision || self.face_recognition)
        {
            return Err(ProvisionError::InvalidCatalog(
                "single-file llama.cpp release has no vision projection",
            ));
        }
        if (self.artifact_format.is_speech()
            || self.artifact_format == ModelArtifactFormat::SileroGgml)
            && self.segmentation
        {
            return Err(ProvisionError::InvalidCatalog(
                "speech graph cannot claim segmentation",
            ));
        }
        if self.runs_in_process() {
            if !self.allowed_runtimes.is_empty() {
                return Err(ProvisionError::InvalidCatalog(
                    "in-process model must not name a vLLM runtime",
                ));
            }
            if self.context_window != 0 || self.vision || self.face_recognition {
                return Err(ProvisionError::InvalidCatalog(
                    "in-process model cannot claim chat, vision or face capabilities",
                ));
            }
            if self.in_process_capabilities().is_empty() {
                return Err(ProvisionError::InvalidCatalog(
                    "in-process model declares no capability",
                ));
            }
        } else {
            if self.allowed_runtimes.is_empty() {
                return Err(ProvisionError::InvalidCatalog("model supports no runtime"));
            }
            if self.context_window == 0 {
                return Err(ProvisionError::InvalidCatalog(
                    "runtime model needs a context window",
                ));
            }
            if self.segmentation {
                return Err(ProvisionError::InvalidCatalog(
                    "segmentation is only available to in-process models",
                ));
            }
        }
        Ok(())
    }
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
            runtime.validate_execution()?;
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
            model.validate_shape()?;
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

    /// Select a hardware-compatible engine for this explicit model layout.
    pub fn runtime_for_model(
        &self,
        kind: RuntimeKind,
        model: &LocalModelRelease,
    ) -> Option<&RuntimeRelease> {
        self.runtimes
            .iter()
            .filter(|runtime| {
                runtime.kind == kind
                    && model.allowed_runtimes.contains(&kind)
                    && runtime.supports_format(model.artifact_format)
            })
            .max_by_key(|runtime| &runtime.version)
    }

    /// Pick the largest known-fitting model for the preferred runtime. The
    /// caller still presents this as a recommendation, never a silent install.
    /// In-process models are never the chat recommendation.
    pub fn recommend<'a>(
        &'a self,
        hardware: &HardwareSnapshot,
        runtime: RuntimeKind,
    ) -> Option<&'a LocalModelRelease> {
        let accelerator = accelerator_memory(hardware, runtime);
        self.models
            .iter()
            .filter(|model| {
                !model.runs_in_process()
                    && (runtime != RuntimeKind::Cpu
                        || (hardware.cpu_threads >= 4 && hardware.memory_mib >= 8192))
                    && model.allowed_runtimes.contains(&runtime)
                    && self.runtime_for_model(runtime, model).is_some()
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
            (RuntimeKind::NvidiaCuda, Accelerator::Nvidia { vram_mib, driver })
                if !driver.trim().is_empty() =>
            {
                Some(*vram_mib)
            }
            (
                RuntimeKind::AmdRocm,
                Accelerator::Amd {
                    vram_mib,
                    rocm: Some(version),
                    ..
                },
            ) if !version.trim().is_empty() => Some(*vram_mib),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Select an explicit model using its declared capacity and compatible pinned engines.
/// Hardware facts are used exactly as observed, without rounding VRAM up. The
/// caller supplies only kinds whose signed runtime supports the model's format.
/// A qualifying accelerator without its matching runtime is an error; selecting
/// CPU cannot silently bypass an inactive driver or accelerator requirement.
pub fn select_runtime_for_model(
    hardware: &HardwareSnapshot,
    model: &LocalModelRelease,
    compatible: impl IntoIterator<Item = RuntimeKind>,
) -> RuntimeDecision {
    if model.required_memory_mib > hardware.memory_mib {
        return RuntimeDecision::Unavailable(
            "The selected model needs more RAM than is available.",
        );
    }
    let compatible: BTreeSet<_> = compatible.into_iter().collect();
    for kind in [RuntimeKind::NvidiaCuda, RuntimeKind::AmdRocm] {
        if model.allowed_runtimes.contains(&kind)
            && hardware
                .accelerators
                .iter()
                .any(|accelerator| match (kind, accelerator) {
                    (RuntimeKind::NvidiaCuda, Accelerator::Nvidia { vram_mib, driver }) => {
                        *vram_mib >= model.required_accelerator_mib && !driver.trim().is_empty()
                    }
                    (
                        RuntimeKind::AmdRocm,
                        Accelerator::Amd {
                            vram_mib,
                            rocm: Some(version),
                        },
                    ) => *vram_mib >= model.required_accelerator_mib && !version.trim().is_empty(),
                    _ => false,
                })
        {
            return if compatible.contains(&kind) {
                RuntimeDecision::Ready(kind)
            } else {
                RuntimeDecision::Unavailable(
                    "The selected accelerator has no compatible signed runtime. Install the matching signed driver boot pack and restart before local inference.",
                )
            };
        }
    }
    if hardware
        .accelerators
        .iter()
        .any(|accelerator| match accelerator {
            Accelerator::Nvidia { driver, .. } => driver.trim().is_empty(),
            Accelerator::Amd { rocm, .. } => rocm
                .as_ref()
                .is_none_or(|version| version.trim().is_empty()),
        })
    {
        return RuntimeDecision::Unavailable(
            "An accelerator was detected but its signed driver runtime is inactive. Install the matching signed driver boot pack and restart before local inference.",
        );
    }
    if model.required_accelerator_mib > 0 {
        return RuntimeDecision::Unavailable(
            "The selected model needs more compatible accelerator memory than is available.",
        );
    }
    if !model.allowed_runtimes.contains(&RuntimeKind::Cpu)
        || !compatible.contains(&RuntimeKind::Cpu)
    {
        return RuntimeDecision::Unavailable(
            "The selected model has no compatible signed CPU runtime.",
        );
    }
    if hardware.memory_mib < 8192 || hardware.cpu_threads < 4 {
        return RuntimeDecision::Unavailable(
            "Local CPU inference needs at least 8 GiB RAM and four CPU threads.",
        );
    }
    RuntimeDecision::Ready(RuntimeKind::Cpu)
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
        let release = catalog
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or(ProvisionError::InvalidCatalog("unknown model id"))?;
        let runtime = match select_runtime_for_model(
            hardware,
            release,
            catalog
                .runtimes
                .iter()
                .filter(|runtime| runtime.supports_format(release.artifact_format))
                .map(|runtime| runtime.kind),
        ) {
            RuntimeDecision::Ready(runtime) => runtime,
            RuntimeDecision::Unavailable(reason) => {
                return Err(ProvisionError::UnsupportedHardware(reason.into()));
            }
        };
        if !release.allowed_runtimes.contains(&runtime) {
            return Err(ProvisionError::UnsupportedHardware(format!(
                "{} cannot run on {}",
                release.label,
                runtime.label()
            )));
        }
        let runtime_release =
            catalog
                .runtime_for_model(runtime, release)
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
        catalog.validate()?;
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or(ProvisionError::InvalidCatalog("unknown model id"))?;
        let runtime = match select_runtime_for_model(
            hardware,
            model,
            catalog
                .runtimes
                .iter()
                .filter(|runtime| runtime.supports_format(model.artifact_format))
                .map(|runtime| runtime.kind),
        ) {
            RuntimeDecision::Ready(runtime) => runtime,
            RuntimeDecision::Unavailable(reason) => {
                return Err(ProvisionError::UnsupportedHardware(reason.into()));
            }
        };
        let runtime_release =
            catalog
                .runtime_for_model(runtime, model)
                .ok_or(ProvisionError::InvalidCatalog(
                    "no compatible pinned runtime for selected model",
                ))?;
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

    #[test]
    fn legacy_runtime_metadata_roundtrips_without_changing_signing_payload() {
        let runtime = catalog().runtimes.remove(0);
        // Exact legacy RuntimeRelease encoding, used inside signed payloads.
        let golden = r#"{"id":"vllm-cpu-0.9.0","kind":"cpu","version":"0.9.0","artifact":{"url":"https://models.example.invalid/runtime","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size_bytes":100},"oci_image":"registry.example.invalid/tilo/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
        assert_eq!(serde_json::to_string(&runtime).unwrap(), golden);
        let mut wire = serde_json::to_value(&runtime).unwrap();
        assert!(wire.get("execution").is_none());
        let recovered: RuntimeRelease = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(recovered).unwrap(), wire);
        wire["execution"] =
            serde_json::json!({"engine":"llama_cpp","max_context_tokens":4096,"gpu_layers":0});
        let configured: RuntimeRelease = serde_json::from_value(wire).unwrap();
        assert!(configured.supports_format(ModelArtifactFormat::LlamaCppGguf));
        assert!(!configured.supports_format(ModelArtifactFormat::VllmGguf));
        let invalid = serde_json::json!({"engine":"llama_cpp","max_context_tokens":4096,"gpu_layers":0,"args":["--host","0.0.0.0"]});
        assert!(serde_json::from_value::<RuntimeExecution>(invalid).is_err());
    }

    #[test]
    fn parallel_slots_preserve_legacy_bytes_and_reject_malformed_counts() {
        let golden = r#"{"engine":"llama_cpp","max_context_tokens":4096,"gpu_layers":0}"#;
        let execution: RuntimeExecution = serde_json::from_str(golden).unwrap();
        assert_eq!(serde_json::to_string(&execution).unwrap(), golden);
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(65536),
            serde_json::json!(true),
            serde_json::json!("1"),
        ] {
            let mut wire: serde_json::Value = serde_json::from_str(golden).unwrap();
            wire["parallel_slots"] = value;
            assert!(serde_json::from_value::<RuntimeExecution>(wire).is_err());
        }
        let mut runtime = catalog().runtimes.remove(0);
        for slots in [1, 64, 65] {
            runtime.execution = Some(RuntimeExecution::LlamaCpp {
                max_context_tokens: 4096,
                gpu_layers: 0,
                parallel_slots: NonZeroU16::new(slots),
            });
            assert_eq!(runtime.validate_execution().is_ok(), slots <= 64);
        }
    }

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
                execution: None,
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
                segmentation: false,
            }],
        }
    }
    fn segmentation_model() -> LocalModelRelease {
        LocalModelRelease {
            id: "subject".into(),
            label: "Subject".into(),
            description: "A tiny segmentation test model".into(),
            version: "b".repeat(64),
            artifact: artifact("subject.onnx"),
            artifact_format: ModelArtifactFormat::OnnxFile,
            license_id: "Apache-2.0".into(),
            license_url: "https://models.example.invalid/LICENSE".into(),
            requires_acceptance: true,
            installed_size_bytes: 100,
            allowed_runtimes: BTreeSet::new(),
            required_memory_mib: 1024,
            required_accelerator_mib: 0,
            context_window: 0,
            vision: false,
            face_recognition: false,
            segmentation: true,
        }
    }

    #[test]
    fn whistle_weights_declare_speech_without_executable_runtime_or_chat_capabilities() {
        let mut speech = segmentation_model();
        speech.artifact_format = ModelArtifactFormat::WhistleCact;
        speech.segmentation = false;
        speech.validate_shape().unwrap();
        assert_eq!(
            speech.in_process_capabilities(),
            [InProcessCapability::SpeechRecognition]
        );
        assert_eq!(
            serde_json::to_string(&speech.artifact_format).unwrap(),
            "\"whistle_cact\""
        );
        speech.segmentation = true;
        assert!(speech.validate_shape().is_err());
        speech.segmentation = false;
        speech.context_window = 100;
        assert!(speech.validate_shape().is_err());
        speech.context_window = 0;
        speech.allowed_runtimes.insert(RuntimeKind::Cpu);
        assert!(speech.validate_shape().is_err());
    }

    #[test]
    fn acoustic_graph_never_advertises_transcription_or_permission() {
        let mut graph = segmentation_model();
        graph.artifact_format = ModelArtifactFormat::SileroGgml;
        graph.segmentation = false;
        graph.validate_shape().unwrap();
        assert_eq!(
            graph.in_process_capabilities(),
            vec![InProcessCapability::SpeechActivity]
        );
        assert!(!graph.artifact_format.is_speech());
        assert_eq!(
            serde_json::to_string(&graph.artifact_format).unwrap(),
            "\"silero_ggml\""
        );
        graph.segmentation = true;
        assert!(graph.validate_shape().is_err());
        graph.segmentation = false;
        graph.allowed_runtimes.insert(RuntimeKind::Cpu);
        assert!(graph.validate_shape().is_err());
    }

    #[test]
    fn speech_graph_is_native_and_never_a_chat_or_segmentation_model() {
        let mut speech = segmentation_model();
        speech.id = "speech".into();
        speech.artifact_format = ModelArtifactFormat::WhisperGgml;
        speech.segmentation = false;
        speech.validate_shape().unwrap();
        assert_eq!(
            speech.in_process_capabilities(),
            [InProcessCapability::SpeechRecognition]
        );
        let mut catalog = catalog();
        catalog.models.push(speech.clone());
        catalog.validate().unwrap();
        assert_eq!(
            catalog.recommend(&cpu(), RuntimeKind::Cpu).unwrap().id,
            "tiny-q4"
        );
        for contradiction in 0..5 {
            let mut invalid = speech.clone();
            match contradiction {
                0 => invalid.context_window = 1,
                1 => invalid.allowed_runtimes = [RuntimeKind::Cpu].into(),
                2 => invalid.vision = true,
                3 => invalid.face_recognition = true,
                _ => invalid.segmentation = true,
            }
            assert!(invalid.validate_shape().is_err());
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

    /// Installation resumes then verifies before each install; activation refuses a network-facing
    /// `vLLM` endpoint.
    #[test]
    fn local_model_setup_follows_the_hardware_and_verifies_before_activating() {
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

    /// An ONNX model runs in-process: it is valid without a runtime or
    /// context window, never becomes the chat recommendation, and cannot
    /// borrow a runtime model's capabilities or lend its own to one.
    #[test]
    fn in_process_models_need_no_runtime_and_claim_only_their_capability() {
        let mut catalog = catalog();
        catalog.models.push(segmentation_model());
        catalog.validate().unwrap();
        assert_eq!(
            catalog
                .recommend(&cpu(), RuntimeKind::Cpu)
                .map(|model| model.id.as_str()),
            Some("tiny-q4")
        );
        catalog.models.retain(|model| model.id == "subject");
        assert_eq!(catalog.recommend(&cpu(), RuntimeKind::Cpu), None);
        assert_eq!(
            segmentation_model().in_process_capabilities(),
            [InProcessCapability::Segmentation]
        );

        let mut with_runtime = segmentation_model();
        with_runtime.allowed_runtimes = [RuntimeKind::Cpu].into();
        assert!(with_runtime.validate_shape().is_err());
        let mut chat = segmentation_model();
        chat.context_window = 4096;
        assert!(chat.validate_shape().is_err());
        let mut vision = segmentation_model();
        vision.vision = true;
        assert!(vision.validate_shape().is_err());
        let mut face = segmentation_model();
        face.face_recognition = true;
        assert!(face.validate_shape().is_err());
        let mut nothing = segmentation_model();
        nothing.segmentation = false;
        assert!(nothing.validate_shape().is_err());

        let mut runtime_segmentation = catalog_model();
        runtime_segmentation.segmentation = true;
        assert!(runtime_segmentation.validate_shape().is_err());
        let mut no_runtime = catalog_model();
        no_runtime.allowed_runtimes.clear();
        assert!(no_runtime.validate_shape().is_err());
    }

    #[test]
    fn incompatible_engine_and_disallowed_backend_never_recommend_or_activate() {
        let mut value = catalog();
        value.models[0].artifact_format = ModelArtifactFormat::LlamaCppGguf;
        let mut fake = Fake {
            partial: 0,
            calls: vec![],
            endpoint: "http://127.0.0.1:8000/v1".into(),
        };
        assert!(value.recommend(&cpu(), RuntimeKind::Cpu).is_none());
        assert!(
            LocalModelProvisioner::new(&mut fake)
                .activate(&value, &cpu(), "tiny-q4", 8000)
                .is_err()
        );
        assert!(fake.calls.is_empty());
        value.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 0,
            parallel_slots: None,
        });
        assert!(value.recommend(&cpu(), RuntimeKind::Cpu).is_some());
        value.models[0].allowed_runtimes = [RuntimeKind::NvidiaCuda].into();
        assert!(
            value
                .runtime_for_model(RuntimeKind::Cpu, &value.models[0])
                .is_none()
        );
        assert!(
            LocalModelProvisioner::new(&mut fake)
                .activate(&value, &cpu(), "tiny-q4", 8000)
                .is_err()
        );
        assert!(fake.calls.is_empty());
    }

    fn partial_offload_catalog() -> LocalModelCatalog {
        let mut value = catalog();
        value.models[0].artifact_format = ModelArtifactFormat::LlamaCppGguf;
        value.models[0].allowed_runtimes = [RuntimeKind::Cpu, RuntimeKind::NvidiaCuda].into();
        value.models[0].required_accelerator_mib = 4096;
        value.runtimes[0].kind = RuntimeKind::NvidiaCuda;
        value.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 8,
            parallel_slots: None,
        });
        value
    }

    fn mixed_accelerators() -> HardwareSnapshot {
        HardwareSnapshot {
            accelerators: vec![
                Accelerator::Amd {
                    vram_mib: 512,
                    rocm: None,
                },
                Accelerator::Nvidia {
                    vram_mib: 8188,
                    driver: "615.71.09".into(),
                },
            ],
            ..cpu()
        }
    }

    #[test]
    fn explicit_partial_offload_installs_on_exact_8188_mib_despite_inactive_integrated_gpu() {
        let value = partial_offload_catalog();
        let hardware = mixed_accelerators();
        let mut fake = Fake {
            partial: 0,
            calls: vec![],
            endpoint: "http://127.0.0.1:8000/v1".into(),
        };
        assert_eq!(
            LocalModelProvisioner::new(&mut fake)
                .install(&value, &hardware, "tiny-q4")
                .unwrap(),
            LocalModelStatus::Installed
        );
        assert!(matches!(
            LocalModelProvisioner::new(&mut fake)
                .activate(&value, &hardware, "tiny-q4", 8000)
                .unwrap(),
            LocalModelStatus::Ready { .. }
        ));
        assert_eq!(
            value
                .recommend(&hardware, RuntimeKind::NvidiaCuda)
                .unwrap()
                .id,
            "tiny-q4"
        );
        assert!(value.recommend(&hardware, RuntimeKind::Cpu).is_none());
    }

    #[test]
    fn explicit_selection_rejects_missing_capacity_driver_or_engine_before_backend_work() {
        for failure in 0..6 {
            let mut value = partial_offload_catalog();
            let mut hardware = mixed_accelerators();
            match failure {
                0 => value.models[0].required_accelerator_mib = 8189,
                1 => {
                    hardware.accelerators = vec![Accelerator::Nvidia {
                        vram_mib: 8188,
                        driver: " ".into(),
                    }];
                }
                2 => value.runtimes[0].execution = None,
                3 => value.runtimes[0].kind = RuntimeKind::Cpu,
                4 => hardware.memory_mib = 8191,
                _ => value.models[0].allowed_runtimes = [RuntimeKind::Cpu].into(),
            }
            // CPU runtime metadata must remain valid when testing absent GPU kinds.
            if failure == 3 {
                value.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
                    max_context_tokens: 4096,
                    gpu_layers: 0,
                    parallel_slots: None,
                });
            }
            let mut fake = Fake {
                partial: 0,
                calls: vec![],
                endpoint: "http://127.0.0.1:8000/v1".into(),
            };
            assert!(
                LocalModelProvisioner::new(&mut fake)
                    .install(&value, &hardware, "tiny-q4")
                    .is_err(),
                "case {failure}"
            );
            assert!(
                LocalModelProvisioner::new(&mut fake)
                    .activate(&value, &hardware, "tiny-q4", 8000)
                    .is_err(),
                "case {failure}"
            );
            assert!(fake.calls.is_empty(), "case {failure}");
        }
    }

    #[test]
    fn explicit_cpu_selection_requires_real_ram_threads_and_zero_accelerator_requirement() {
        let mut model = catalog_model();
        for failure in 0..3 {
            let mut hardware = cpu();
            match failure {
                0 => hardware.cpu_threads = 3,
                1 => model.required_memory_mib = hardware.memory_mib + 1,
                _ => {
                    model.required_memory_mib = 8192;
                    model.required_accelerator_mib = 1;
                }
            }
            assert!(matches!(
                select_runtime_for_model(&hardware, &model, [RuntimeKind::Cpu]),
                RuntimeDecision::Unavailable(_)
            ));
        }
        let mut underpowered = cpu();
        underpowered.cpu_threads = 3;
        assert!(
            catalog()
                .recommend(&underpowered, RuntimeKind::Cpu)
                .is_none()
        );
        model.required_accelerator_mib = 0;
        assert_eq!(
            select_runtime_for_model(&cpu(), &model, [RuntimeKind::Cpu]),
            RuntimeDecision::Ready(RuntimeKind::Cpu)
        );
    }

    fn catalog_model() -> LocalModelRelease {
        catalog().models.remove(0)
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
            self.calls.push("start");
            Ok(LocalEndpoint {
                url: self.endpoint.clone(),
                model_id: "tiny-q4".into(),
            })
        }
    }
}
