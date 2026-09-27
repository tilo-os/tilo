//! Local model provisioning with no ambient shell access.
//!
//! The desktop decides what package/runtime it trusts through a pinned catalog;
//! this module decides whether it fits the machine and drives only a typed
//! [`LocalModelBackend`]. The backend may use a downloader, OCI manager, and
//! service manager, but this layer never constructs a command line from model
//! metadata or user input.
//!
//! No engine is a default. [`LocalModelCatalog::configurations`] ranks every
//! (engine, device, model) combination the signed catalogue and the probed
//! hardware support, each with an honest fit and speed; the best one is the
//! recommendation and the rest are its alternatives.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorVendor {
    Nvidia,
    Amd,
    Intel,
}

impl AcceleratorVendor {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA",
            Self::Amd => "AMD",
            Self::Intel => "Intel",
        }
    }
}

/// One GPU the trusted hardware probe observed on the PCI bus. A UI or agent
/// never infers this from a user supplied PCI string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    pub vendor: AcceleratorVendor,
    /// The product name from the vendor's own tables; empty when unknown.
    pub name: String,
    /// PCI address (`0000:66:00.0`): the stable identity a configuration
    /// names.
    pub pci_slot: String,
    /// DRM render node (`renderD128`) the device is reached through.
    pub render_node: Option<String>,
    /// Kernel driver bound to the device (`amdgpu`, `nvidia`, `i915`, `xe`);
    /// empty when none is.
    pub kernel_driver: String,
    /// Part of the processor: its model memory is system RAM.
    pub integrated: bool,
    /// Memory that belongs to the device alone: a card's VRAM, or an
    /// integrated GPU's firmware carve-out.
    pub dedicated_mib: u64,
    /// System memory the device may map on top (GTT). It is the RAM the
    /// desktop runs in, so it is counted against what is free.
    pub shared_mib: u64,
    /// The vendor compute stack answering for the device: the NVIDIA driver
    /// version, or the `ROCm` version. `None` when none answers.
    pub compute: Option<String>,
    /// Present and set up, but usable only after the next restart: the
    /// loaded kernel module and the userspace library disagree, or a driver
    /// deployment is staged for the next boot. Distinct from an absent card,
    /// so nothing degrades to CPU or a hosted model as if there were no GPU.
    pub restart: Option<RestartReason>,
}

impl Accelerator {
    /// The name a person knows the device by.
    #[must_use]
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() {
            format!("{} graphics", self.vendor.label())
        } else {
            self.name.clone()
        }
    }

    fn compute_answers(&self) -> bool {
        self.restart.is_none()
            && self
                .compute
                .as_deref()
                .is_some_and(|version| !version.trim().is_empty())
    }

    /// Whether the runtime `kind` can run a model on this device. Vulkan
    /// needs no vendor stack on the host for AMD and Intel: the runtime pack
    /// carries Mesa, which drives their in-kernel drivers through the render
    /// node. NVIDIA's own Vulkan driver comes with the card's container
    /// device, so it needs a driver that answers.
    #[must_use]
    pub fn drives(&self, kind: RuntimeKind) -> bool {
        match kind {
            RuntimeKind::Cpu => false,
            RuntimeKind::NvidiaCuda => {
                self.vendor == AcceleratorVendor::Nvidia && self.compute_answers()
            }
            RuntimeKind::AmdRocm => {
                self.vendor == AcceleratorVendor::Amd && !self.integrated && self.compute_answers()
            }
            RuntimeKind::Vulkan => {
                self.restart.is_none()
                    && match self.vendor {
                        AcceleratorVendor::Nvidia => {
                            self.kernel_driver == "nvidia" && self.compute_answers()
                        }
                        AcceleratorVendor::Amd => {
                            self.kernel_driver == "amdgpu" && self.render_node.is_some()
                        }
                        AcceleratorVendor::Intel => {
                            matches!(self.kernel_driver.as_str(), "i915" | "xe")
                                && self.render_node.is_some()
                        }
                    }
            }
        }
    }

    /// Memory a model may take on this device. A card's own VRAM is its
    /// budget. An integrated GPU has only a small carve-out of its own; the
    /// rest is system RAM it maps, so it gets no more of that than the
    /// desktop can spare right now.
    #[must_use]
    pub fn model_budget_mib(&self, hardware: &HardwareSnapshot) -> u64 {
        if self.integrated {
            self.dedicated_mib
                .saturating_add(self.shared_mib.min(hardware.cpu_model_budget_mib()))
        } else {
            self.dedicated_mib
        }
    }

    /// A driverless device: present on the bus, with nothing to run it.
    #[must_use]
    pub fn needs_driver(&self) -> bool {
        self.restart.is_none()
            && (self.kernel_driver.trim().is_empty()
                || (self.vendor == AcceleratorVendor::Nvidia && !self.compute_answers()))
    }
}

/// Why a present accelerator waits for a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartReason {
    /// The running kernel module and the installed userspace driver differ
    /// in version (NVML reports a driver/library mismatch).
    DriverLibraryMismatch,
    /// A signed driver deployment is staged and boots next time.
    DriverStaged,
}

/// The one accelerator fact every consumer asks about first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceleratorState {
    /// A driver answers and the card is usable now.
    Usable { kind: RuntimeKind, vram_mib: u64 },
    /// Present and set up; usable after a restart.
    RestartRequired {
        vendor: AcceleratorVendor,
        reason: RestartReason,
    },
    /// Present on the bus with no working driver; setup is needed.
    NeedsDriver { vendor: AcceleratorVendor },
    /// No supported accelerator.
    Absent,
}

/// The capability snapshot a privileged hardware service publishes. A UI or
/// agent never infers this from a user supplied PCI string.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HardwareSnapshot {
    pub cpu_threads: u16,
    pub memory_mib: u64,
    /// Memory the kernel reports available for new work (`MemAvailable`),
    /// the budget a CPU model competes for with the desktop.
    pub available_memory_mib: u64,
    pub free_storage_mib: u64,
    pub accelerators: Vec<Accelerator>,
}

impl HardwareSnapshot {
    /// The discrete card this device offers, strongest state first: a usable
    /// card wins over one waiting for a restart, which wins over one with no
    /// driver. An integrated GPU shares the desktop's RAM and never counts as
    /// a card here, though a configuration may still run on it.
    #[must_use]
    pub fn accelerator_state(&self) -> AcceleratorState {
        let usable = self
            .accelerators
            .iter()
            .filter(|accelerator| {
                !accelerator.integrated
                    && accelerator_class_gib(accelerator.dedicated_mib) >= MIN_ACCELERATOR_GIB
            })
            .filter_map(|accelerator| {
                [
                    RuntimeKind::NvidiaCuda,
                    RuntimeKind::AmdRocm,
                    RuntimeKind::Vulkan,
                ]
                .into_iter()
                .find(|kind| accelerator.drives(*kind))
                .map(|kind| (kind, accelerator.dedicated_mib))
            })
            .max_by_key(|(kind, vram)| (*kind == RuntimeKind::NvidiaCuda, *vram));
        if let Some((kind, vram_mib)) = usable {
            return AcceleratorState::Usable { kind, vram_mib };
        }
        let restart = self.accelerators.iter().find_map(|accelerator| {
            accelerator
                .restart
                .map(|reason| (accelerator.vendor, reason))
        });
        if let Some((vendor, reason)) = restart {
            return AcceleratorState::RestartRequired { vendor, reason };
        }
        match self
            .accelerators
            .iter()
            .find(|accelerator| accelerator.needs_driver())
        {
            Some(accelerator) => AcceleratorState::NeedsDriver {
                vendor: accelerator.vendor,
            },
            None => AcceleratorState::Absent,
        }
    }

    /// Memory a CPU model may take after leaving room for the desktop.
    #[must_use]
    pub fn cpu_model_budget_mib(&self) -> u64 {
        let available = if self.available_memory_mib == 0 {
            self.memory_mib
        } else {
            self.available_memory_mib
        };
        available.saturating_sub(DESKTOP_HEADROOM_MIB)
    }

    /// The probed device at `pci_slot`.
    #[must_use]
    pub fn accelerator(&self, pci_slot: &str) -> Option<&Accelerator> {
        self.accelerators
            .iter()
            .find(|accelerator| accelerator.pci_slot == pci_slot)
    }
}

/// Memory kept free for the desktop and open apps when a model runs in RAM.
pub const DESKTOP_HEADROOM_MIB: u64 = 4096;

/// Fewest CPU threads a CPU model is offered on.
pub const MIN_CPU_THREADS: u16 = 4;

/// How quickly a local model answers on this device, as the person
/// experiences it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedClass {
    /// Too slow to offer: minutes for an ordinary answer.
    Impractical,
    /// Usable for background work; an answer takes tens of seconds.
    Slow,
    /// Responsive enough to wait for.
    Interactive,
}

/// An honest throughput estimate for one model on one runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedSpeed {
    pub tokens_per_second: u16,
    pub class: SpeedClass,
}

/// Token generation is bound by how fast the weights stream from memory:
/// roughly the memory bandwidth divided by the bytes read per token (the
/// quantized weights). Desktop DDR sustains about 25 GiB/s to eight or more
/// CPU threads, and an integrated GPU reading the same RAM about twice that.
/// A card's own memory is an order of magnitude faster; llama.cpp's Vulkan
/// backend reaches somewhat less of it than a CUDA or `ROCm` engine.
#[must_use]
pub fn expected_speed(
    hardware: &HardwareSnapshot,
    kind: RuntimeKind,
    device: Option<&Accelerator>,
    model: &LocalModelRelease,
) -> ExpectedSpeed {
    let size_mib =
        u32::try_from(model.artifact.download_bytes() / (1024 * 1024)).unwrap_or(u32::MAX);
    let weights_gib = (f64::from(size_mib) / 1024.0).max(0.25);
    let bandwidth_gib_s = match (kind, device) {
        (RuntimeKind::Cpu, _) | (_, None) => 25.0 * (f64::from(hardware.cpu_threads.min(8)) / 8.0),
        (_, Some(device)) if device.integrated => 50.0,
        (RuntimeKind::Vulkan, Some(_)) => 240.0,
        (RuntimeKind::NvidiaCuda | RuntimeKind::AmdRocm, Some(_)) => 300.0,
    };
    let tokens = (bandwidth_gib_s / weights_gib).clamp(0.0, f64::from(u16::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the non-negative u16 range above"
    )]
    let tokens_per_second = tokens as u16;
    let class = match tokens_per_second {
        0..=2 => SpeedClass::Impractical,
        3..=14 => SpeedClass::Slow,
        _ => SpeedClass::Interactive,
    };
    ExpectedSpeed {
        tokens_per_second,
        class,
    }
}

/// The processor a runtime runs a model on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Cpu,
    NvidiaCuda,
    AmdRocm,
    /// Any GPU with a Vulkan driver: AMD and Intel through the Mesa drivers
    /// the runtime pack carries, NVIDIA through its own.
    Vulkan,
}

impl RuntimeKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::NvidiaCuda => "NVIDIA CUDA",
            Self::AmdRocm => "AMD ROCm",
            Self::Vulkan => "Vulkan",
        }
    }
}

/// The inference engine a runtime image carries. The model manager builds
/// the engine's fixed argv from signed catalogue fields; the catalogue never
/// carries a command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEngine {
    /// vLLM's OpenAI-compatible server, on a Hugging Face snapshot.
    Vllm,
    /// llama.cpp's `llama-server`, on a single GGUF file.
    LlamaCpp,
}

impl RuntimeEngine {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Vllm => "vLLM",
            Self::LlamaCpp => "llama.cpp",
        }
    }

    /// Whether a build of this engine can run on `kind`.
    #[must_use]
    pub const fn serves(self, kind: RuntimeKind) -> bool {
        match self {
            Self::Vllm => matches!(
                kind,
                RuntimeKind::NvidiaCuda | RuntimeKind::AmdRocm | RuntimeKind::Cpu
            ),
            Self::LlamaCpp => matches!(kind, RuntimeKind::Vulkan | RuntimeKind::Cpu),
        }
    }

    /// Whether this engine loads a release laid out as `artifact`.
    #[must_use]
    pub const fn loads(self, artifact: &ModelArtifact) -> bool {
        matches!(
            (self, artifact),
            (Self::Vllm, ModelArtifact::HfSnapshot { .. })
                | (Self::LlamaCpp, ModelArtifact::Gguf(_))
        )
    }
}

/// A pinned runtime pack: an OCI image in its vendor's registry, pulled by
/// digest.
///
/// `artifact` pins the image manifest itself (its registry URL, SHA-256 and
/// length), so `oci_image` names exactly that digest and the signed extension
/// record covers the same bytes the container engine verifies on pull. The
/// image is never mirrored or repackaged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRelease {
    pub id: String,
    /// The processors this build runs models on.
    pub kinds: BTreeSet<RuntimeKind>,
    pub version: String,
    pub engine: RuntimeEngine,
    pub artifact: PinnedArtifact,
    /// `registry/repository@sha256:<manifest digest>`.
    pub oci_image: String,
}

impl RuntimeRelease {
    /// The manifest digest `oci_image` names, when it is digest-pinned.
    #[must_use]
    pub fn image_digest(&self) -> Option<&str> {
        let (repository, digest) = self.oci_image.rsplit_once("@sha256:")?;
        (!repository.is_empty() && is_sha256(digest)).then_some(digest)
    }
}

/// An immutable, verified downloadable object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedArtifact {
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        if !is_sha256(&self.sha256) {
            return Err(ProvisionError::InvalidCatalog(
                "artifact SHA-256 must be 64 lowercase hex characters",
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

/// One file of a model snapshot, at its path inside the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub artifact: PinnedArtifact,
}

/// Files a snapshot may hold; far above any real model repository.
pub const MAX_SNAPSHOT_FILES: usize = 4_096;

/// Whether `path` is a plain relative path inside a snapshot.
#[must_use]
pub fn is_snapshot_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1_024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A model release's bytes and how they are laid out. The layout is signed
/// catalogue data, never guessed from a filename or HTTP header, and every
/// byte is fetched from the vendor's own source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelArtifact {
    /// A Hugging Face snapshot at a pinned revision, fetched file by file
    /// from the repository's own resolve URLs and assembled into a directory.
    HfSnapshot { files: Vec<SnapshotFile> },
    /// A single GGUF file.
    Gguf(PinnedArtifact),
}

impl ModelArtifact {
    /// The pinned objects to fetch.
    #[must_use]
    pub fn objects(&self) -> Vec<&PinnedArtifact> {
        match self {
            Self::HfSnapshot { files } => files.iter().map(|file| &file.artifact).collect(),
            Self::Gguf(artifact) => vec![artifact],
        }
    }

    /// Bytes to download.
    #[must_use]
    pub fn download_bytes(&self) -> u64 {
        self.objects().iter().fold(0_u64, |total, artifact| {
            total.saturating_add(artifact.size_bytes)
        })
    }

    /// The release's content identity: the object's SHA-256 for a single
    /// file, or the SHA-256 of the sorted `path NUL sha256 NUL size LF`
    /// listing for a snapshot. Installed directories and license acceptances
    /// are keyed by it.
    #[must_use]
    pub fn identity(&self) -> String {
        use sha2::{Digest as _, Sha256};
        match self {
            Self::Gguf(artifact) => artifact.sha256.clone(),
            Self::HfSnapshot { files } => {
                let mut listing: Vec<&SnapshotFile> = files.iter().collect();
                listing.sort_by(|left, right| left.path.cmp(&right.path));
                let mut hasher = Sha256::new();
                for file in listing {
                    hasher.update(file.path.as_bytes());
                    hasher.update([0]);
                    hasher.update(file.artifact.sha256.as_bytes());
                    hasher.update([0]);
                    hasher.update(file.artifact.size_bytes.to_string().as_bytes());
                    hasher.update(b"\n");
                }
                hex::encode(hasher.finalize())
            }
        }
    }

    /// Validate every pinned object and, for a snapshot, its paths.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid object, an empty or oversized
    /// snapshot, an unsafe or duplicate path, or a snapshot without a model
    /// configuration.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        for artifact in self.objects() {
            artifact.validate()?;
        }
        if let Self::HfSnapshot { files } = self {
            if files.is_empty() || files.len() > MAX_SNAPSHOT_FILES {
                return Err(ProvisionError::InvalidCatalog(
                    "a snapshot holds 1 through 4096 files",
                ));
            }
            let mut paths = BTreeSet::new();
            for file in files {
                if !is_snapshot_path(&file.path) || !paths.insert(file.path.as_str()) {
                    return Err(ProvisionError::InvalidCatalog(
                        "snapshot paths must be unique plain relative paths",
                    ));
                }
            }
            if !paths.contains("config.json") {
                return Err(ProvisionError::InvalidCatalog(
                    "a snapshot must contain config.json",
                ));
            }
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
    pub artifact: ModelArtifact,
    /// SPDX license identifier where one exists (`Apache-2.0`, `MIT`, ...).
    pub license_id: String,
    /// Immutable model-card or license document for the pinned revision.
    pub license_url: String,
    /// Whether setup must capture explicit license acceptance.
    pub requires_acceptance: bool,
    /// Expected on-disk footprint after unpacking/conversion.
    pub installed_size_bytes: u64,
    /// The engines that load this release.
    pub engines: BTreeSet<RuntimeEngine>,
    pub allowed_runtimes: BTreeSet<RuntimeKind>,
    /// RAM the model needs: its whole footprint on the CPU, the engine's
    /// host process beside a card.
    pub required_memory_mib: u64,
    /// Device memory the weights, cache and buffers need on a GPU.
    pub required_accelerator_mib: u64,
    pub context_window: u32,
    /// Signed statement of the fixed gateway protocols this exact revision
    /// serves. The model manager propagates it into provider metadata; no app
    /// guesses a capability from a model id or a manual label.
    #[serde(default)]
    pub capabilities: BTreeSet<crate::ModelCapability>,
    /// Share of the accelerator's memory, in percent, the runtime may claim
    /// for weights and KV cache. `None` keeps the runtime's default, which
    /// assumes a dedicated card; a model meant to run beside the desktop sets
    /// this so the card keeps headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator_memory_percent: Option<u8>,
}

/// Catalogs are supplied by a signed/pinned desktop update channel. They are
/// data, not arbitrary package recipes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalModelCatalog {
    pub runtimes: Vec<RuntimeRelease>,
    pub models: Vec<LocalModelRelease>,
}

/// Where one configuration runs: the runtime pack, the processor and, on a
/// GPU, which one. It names catalogue and probe identities only; the model
/// manager re-derives the configuration from both before acting on it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Placement {
    /// The signed runtime pack's id.
    pub runtime: String,
    pub kind: RuntimeKind,
    /// The GPU's PCI slot; `None` on the CPU.
    pub device: Option<String>,
}

impl Placement {
    /// A stable text key for this placement of `model`, for resource ids.
    #[must_use]
    pub fn key(&self, model: &str) -> String {
        let kind = match self.kind {
            RuntimeKind::Cpu => "cpu",
            RuntimeKind::NvidiaCuda => "nvidia_cuda",
            RuntimeKind::AmdRocm => "amd_rocm",
            RuntimeKind::Vulkan => "vulkan",
        };
        format!(
            "{model}@{}/{kind}/{}",
            self.runtime,
            self.device.as_deref().unwrap_or("cpu")
        )
    }
}

/// How much room a configuration leaves where it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// It fits, with little to spare.
    Tight,
    /// It uses at most four fifths of what it may take.
    Good,
    /// It uses at most half.
    Excellent,
}

impl Fit {
    /// The fit of `required` MiB in a `budget` of MiB, or `None` when it does
    /// not fit at all.
    #[must_use]
    pub fn of(required: u64, budget: u64) -> Option<Self> {
        let (required, budget) = (u128::from(required), u128::from(budget));
        if required.saturating_mul(2) <= budget {
            Some(Self::Excellent)
        } else if required.saturating_mul(5) <= budget.saturating_mul(4) {
            Some(Self::Good)
        } else if required <= budget {
            Some(Self::Tight)
        } else {
            None
        }
    }
}

/// One way this device can run a model: an engine's runtime pack, the
/// processor it runs on, and how honestly it fits there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration<'a> {
    pub runtime: &'a RuntimeRelease,
    pub model: &'a LocalModelRelease,
    pub kind: RuntimeKind,
    /// The GPU; `None` on the CPU.
    pub device: Option<&'a Accelerator>,
    /// Memory the model needs where it runs, in MiB.
    pub required_mib: u64,
    /// Memory it may take there without starving the desktop, in MiB.
    pub budget_mib: u64,
    /// Whether that memory is the RAM the desktop runs in.
    pub shared_memory: bool,
    pub fit: Fit,
    pub speed: ExpectedSpeed,
}

impl Configuration<'_> {
    #[must_use]
    pub fn placement(&self) -> Placement {
        Placement {
            runtime: self.runtime.id.clone(),
            kind: self.kind,
            device: self.device.map(|device| device.pci_slot.clone()),
        }
    }
}

impl LocalModelCatalog {
    /// Validate the runtime and model entries supplied by the signed catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry is incomplete, not pinned, conflicts
    /// with another catalog entry, or cannot run on any runtime here.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        let mut runtime_ids = BTreeSet::new();
        for runtime in &self.runtimes {
            if runtime.id.trim().is_empty() || !runtime_ids.insert(runtime.id.as_str()) {
                return Err(ProvisionError::InvalidCatalog("runtime ids must be unique"));
            }
            runtime.artifact.validate()?;
            if runtime.image_digest() != Some(runtime.artifact.sha256.as_str()) {
                return Err(ProvisionError::InvalidCatalog(
                    "runtime OCI image must be pinned to its signed manifest digest",
                ));
            }
            if runtime.kinds.is_empty()
                || !runtime
                    .kinds
                    .iter()
                    .all(|kind| runtime.engine.serves(*kind))
            {
                return Err(ProvisionError::InvalidCatalog(
                    "a runtime must name processors its engine runs on",
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
            if model.engines.is_empty()
                || !model
                    .engines
                    .iter()
                    .all(|engine| engine.loads(&model.artifact))
            {
                return Err(ProvisionError::InvalidCatalog(
                    "a model must name the engines that load its format",
                ));
            }
            if !self.runtimes.iter().any(|runtime| {
                model.engines.contains(&runtime.engine)
                    && !runtime.kinds.is_disjoint(&model.allowed_runtimes)
            }) {
                return Err(ProvisionError::InvalidCatalog(
                    "a model needs a runtime in the catalogue that runs it",
                ));
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
            if model
                .accelerator_memory_percent
                .is_some_and(|percent| !(10..=95).contains(&percent))
            {
                return Err(ProvisionError::InvalidCatalog(
                    "accelerator memory share must be 10 through 95 percent",
                ));
            }
            if model.context_window == 0 {
                return Err(ProvisionError::InvalidCatalog(
                    "model context window must be non-zero",
                ));
            }
            if model.installed_size_bytes < model.artifact.download_bytes() {
                return Err(ProvisionError::InvalidCatalog(
                    "installed model size cannot be smaller than its artifact",
                ));
            }
        }
        Ok(())
    }

    /// Every configuration that honestly runs here, best first.
    ///
    /// A configuration is a model, a runtime pack whose engine loads it, and
    /// a processor both support. It is offered only when the model fits the
    /// memory it may take there (on the CPU and an integrated GPU, the RAM
    /// the desktop can spare), storage holds it, and it answers faster than
    /// [`SpeedClass::Impractical`]. Faster comes first, then more room.
    ///
    /// A card waiting for a restart holds the decision: nothing is offered
    /// until it boots, so nothing settles for a slower processor meanwhile.
    #[must_use]
    pub fn configurations<'a>(&'a self, hardware: &'a HardwareSnapshot) -> Vec<Configuration<'a>> {
        if matches!(
            hardware.accelerator_state(),
            AcceleratorState::RestartRequired { .. }
        ) {
            return Vec::new();
        }
        let mut found = Vec::new();
        for runtime in &self.runtimes {
            for model in self.models.iter().filter(|model| {
                model.engines.contains(&runtime.engine)
                    && runtime.engine.loads(&model.artifact)
                    && model.installed_size_bytes / (1024 * 1024) <= hardware.free_storage_mib
            }) {
                for &kind in runtime.kinds.intersection(&model.allowed_runtimes) {
                    let places: Vec<Option<&Accelerator>> = if kind == RuntimeKind::Cpu {
                        if hardware.cpu_threads < MIN_CPU_THREADS {
                            continue;
                        }
                        vec![None]
                    } else {
                        if model.required_memory_mib > hardware.memory_mib {
                            continue;
                        }
                        hardware
                            .accelerators
                            .iter()
                            .filter(|device| device.drives(kind))
                            .map(Some)
                            .collect()
                    };
                    for device in places {
                        let (required_mib, budget_mib, shared_memory) = match device {
                            None => (
                                model.required_memory_mib,
                                hardware.cpu_model_budget_mib(),
                                true,
                            ),
                            Some(device) => (
                                model.required_accelerator_mib,
                                device.model_budget_mib(hardware),
                                device.integrated,
                            ),
                        };
                        let Some(fit) = Fit::of(required_mib, budget_mib) else {
                            continue;
                        };
                        let speed = expected_speed(hardware, kind, device, model);
                        if speed.class == SpeedClass::Impractical {
                            continue;
                        }
                        found.push(Configuration {
                            runtime,
                            model,
                            kind,
                            device,
                            required_mib,
                            budget_mib,
                            shared_memory,
                            fit,
                            speed,
                        });
                    }
                }
            }
        }
        found.sort_by(|left, right| {
            right
                .speed
                .class
                .cmp(&left.speed.class)
                .then(
                    right
                        .speed
                        .tokens_per_second
                        .cmp(&left.speed.tokens_per_second),
                )
                .then(right.fit.cmp(&left.fit))
                .then(right.model.context_window.cmp(&left.model.context_window))
                .then(
                    left.model
                        .artifact
                        .download_bytes()
                        .cmp(&right.model.artifact.download_bytes()),
                )
                .then(left.model.id.cmp(&right.model.id))
                .then(left.placement().cmp(&right.placement()))
        });
        found
    }

    /// The configuration `placement` names for `model_id`, when it still
    /// runs here.
    #[must_use]
    pub fn configuration<'a>(
        &'a self,
        hardware: &'a HardwareSnapshot,
        model_id: &str,
        placement: &Placement,
    ) -> Option<Configuration<'a>> {
        self.configurations(hardware)
            .into_iter()
            .find(|found| found.model.id == model_id && &found.placement() == placement)
    }
}

/// Smallest accelerator class, in GiB, a card counts as this device's
/// accelerator for presets.
const MIN_ACCELERATOR_GIB: u64 = 8;

/// The memory class a card is sold as. Drivers report total VRAM minus what
/// the firmware reserves, so an 8 GiB card reads as e.g. 8188 MiB; a raw
/// `>= 8192` comparison would turn every such card away.
fn accelerator_class_gib(vram_mib: u64) -> u64 {
    vram_mib.saturating_add(512) / 1024
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

/// A running supervised model: the Unix socket its OpenAI-compatible server
/// listens on, and the catalogue id it serves. The runtime has no network;
/// this socket is the only way to reach it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEndpoint {
    pub socket: std::path::PathBuf,
    pub model_id: String,
}

impl LocalEndpoint {
    /// Validate that an activated endpoint is a socket path and identifies a
    /// model.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket path is not absolute or the endpoint
    /// does not identify a model.
    pub fn validate(&self) -> Result<(), ProvisionError> {
        if !self.socket.is_absolute() {
            return Err(ProvisionError::UnsafeEndpoint);
        }
        if self.model_id.trim().is_empty() {
            return Err(ProvisionError::InvalidCatalog("endpoint model id is empty"));
        }
        Ok(())
    }
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
                formatter.write_str("local runtime returned an endpoint that is not a socket path")
            }
        }
    }
}
impl std::error::Error for ProvisionError {}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1_073_741_824;

    fn artifact(name: &str, size_bytes: u64) -> PinnedArtifact {
        PinnedArtifact {
            url: format!("https://models.example.invalid/{name}"),
            sha256: "a".repeat(64),
            size_bytes,
        }
    }

    fn runtime(id: &str, engine: RuntimeEngine, kinds: &[RuntimeKind]) -> RuntimeRelease {
        RuntimeRelease {
            id: id.into(),
            kinds: kinds.iter().copied().collect(),
            version: "1".into(),
            engine,
            artifact: artifact(id, 1_000),
            oci_image: format!("registry.example.invalid/{id}@sha256:{}", "a".repeat(64)),
        }
    }

    /// The review model twice, as the catalogue ships it: a 4-bit AWQ
    /// snapshot vLLM serves on a CUDA card, and a `Q4_K_M` GGUF llama.cpp
    /// serves through Vulkan or on the CPU.
    fn catalog() -> LocalModelCatalog {
        let release =
            |id: &str, artifact: ModelArtifact, engine, kinds: &[RuntimeKind]| LocalModelRelease {
                id: id.into(),
                label: id.into(),
                description: "The review model".into(),
                version: "b".repeat(40),
                installed_size_bytes: artifact.download_bytes(),
                artifact,
                license_id: "Apache-2.0".into(),
                license_url: "https://models.example.invalid/LICENSE".into(),
                requires_acceptance: false,
                engines: [engine].into(),
                allowed_runtimes: kinds.iter().copied().collect(),
                required_memory_mib: 12_288,
                required_accelerator_mib: 6_144,
                context_window: 8_192,
                capabilities: [crate::ModelCapability::TraceReview].into(),
                accelerator_memory_percent: Some(70),
            };
        let awq = release(
            "qwen/qwen3-4b-awq",
            ModelArtifact::HfSnapshot {
                files: vec![
                    SnapshotFile {
                        path: "config.json".into(),
                        artifact: artifact("config.json", 1_000),
                    },
                    SnapshotFile {
                        path: "model.safetensors".into(),
                        artifact: artifact("model.safetensors", 2_669_000_000),
                    },
                ],
            },
            RuntimeEngine::Vllm,
            &[RuntimeKind::NvidiaCuda],
        );
        let mut gguf = release(
            "qwen/qwen3-4b-gguf",
            ModelArtifact::Gguf(artifact("Qwen3-4B-Q4_K_M.gguf", 2_497_280_256)),
            RuntimeEngine::LlamaCpp,
            &[RuntimeKind::Vulkan, RuntimeKind::Cpu],
        );
        gguf.required_memory_mib = 5_120;
        gguf.required_accelerator_mib = 4_096;
        gguf.accelerator_memory_percent = None;
        LocalModelCatalog {
            runtimes: vec![
                runtime("vllm-cuda", RuntimeEngine::Vllm, &[RuntimeKind::NvidiaCuda]),
                runtime(
                    "llama-cpp-vulkan",
                    RuntimeEngine::LlamaCpp,
                    &[RuntimeKind::Vulkan, RuntimeKind::Cpu],
                ),
            ],
            models: vec![awq, gguf],
        }
    }

    fn rtx_4070_mobile() -> Accelerator {
        Accelerator {
            vendor: AcceleratorVendor::Nvidia,
            name: "NVIDIA GeForce RTX 4070 Laptop GPU".into(),
            pci_slot: "0000:01:00.0".into(),
            render_node: Some("renderD129".into()),
            kernel_driver: "nvidia".into(),
            integrated: false,
            dedicated_mib: 8_188,
            shared_mib: 0,
            compute: Some("615.71.09".into()),
            restart: None,
        }
    }

    fn radeon_780m() -> Accelerator {
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

    fn machine(accelerators: Vec<Accelerator>) -> HardwareSnapshot {
        HardwareSnapshot {
            cpu_threads: 16,
            memory_mib: 31_287,
            available_memory_mib: 22_528,
            free_storage_mib: 100 * 1024,
            accelerators,
        }
    }

    fn ranked(catalog: &LocalModelCatalog, hardware: &HardwareSnapshot) -> Vec<String> {
        catalog
            .configurations(hardware)
            .iter()
            .map(|found| {
                format!(
                    "{} {} {}",
                    found.runtime.engine.label(),
                    found
                        .device
                        .map_or_else(|| "CPU".into(), Accelerator::label),
                    found.model.id
                )
            })
            .collect()
    }

    #[test]
    fn an_nvidia_card_beside_an_amd_igpu_ranks_vllm_first_and_offers_every_other_way() {
        let catalog = catalog();
        let hardware = machine(vec![radeon_780m(), rtx_4070_mobile()]);
        assert_eq!(
            ranked(&catalog, &hardware),
            [
                "vLLM NVIDIA GeForce RTX 4070 Laptop GPU qwen/qwen3-4b-awq",
                "llama.cpp NVIDIA GeForce RTX 4070 Laptop GPU qwen/qwen3-4b-gguf",
                "llama.cpp AMD Radeon 780M Graphics qwen/qwen3-4b-gguf",
                "llama.cpp CPU qwen/qwen3-4b-gguf",
            ]
        );
        let found = catalog.configurations(&hardware);
        let [vllm, card, igpu, cpu] = found.as_slice() else {
            panic!("four configurations");
        };
        assert_eq!(vllm.kind, RuntimeKind::NvidiaCuda);
        assert_eq!(
            (vllm.fit, vllm.speed.class),
            (Fit::Good, SpeedClass::Interactive)
        );
        assert!(vllm.speed.tokens_per_second > card.speed.tokens_per_second);
        assert_eq!(card.kind, RuntimeKind::Vulkan);
        assert!(!card.shared_memory);
        // The iGPU's model memory is RAM: its own 512 MiB carve-out plus the
        // shared memory it can map, never more than the desktop can spare.
        assert!(igpu.shared_memory);
        assert_eq!(igpu.budget_mib, 512 + 15_644);
        assert_eq!(igpu.speed.class, SpeedClass::Interactive);
        assert_eq!(cpu.kind, RuntimeKind::Cpu);
        assert_eq!(cpu.speed.class, SpeedClass::Slow);
        // A placement names the device it was ranked for, and resolves back.
        let placement = igpu.placement();
        assert_eq!(placement.device.as_deref(), Some("0000:66:00.0"));
        assert_eq!(
            catalog
                .configuration(&hardware, "qwen/qwen3-4b-gguf", &placement)
                .map(|found| found.device.map(|device| device.pci_slot.clone())),
            Some(Some("0000:66:00.0".into()))
        );
        assert!(
            catalog
                .configuration(&hardware, "qwen/qwen3-4b-awq", &placement)
                .is_none(),
            "vLLM does not run the GGUF placement"
        );
        // With the desktop holding most of the RAM, the iGPU and the CPU fit
        // no longer: shared memory is counted against what is free, so the
        // iGPU could now take its carve-out and 7000 - 4096 MiB of RAM.
        let busy = HardwareSnapshot {
            available_memory_mib: 7_000,
            ..hardware
        };
        assert_eq!(
            ranked(&catalog, &busy),
            [
                "vLLM NVIDIA GeForce RTX 4070 Laptop GPU qwen/qwen3-4b-awq",
                "llama.cpp NVIDIA GeForce RTX 4070 Laptop GPU qwen/qwen3-4b-gguf",
            ]
        );
    }

    #[test]
    fn an_igpu_alone_runs_the_gguf_through_vulkan_before_the_cpu() {
        let catalog = catalog();
        let hardware = machine(vec![radeon_780m()]);
        assert_eq!(
            ranked(&catalog, &hardware),
            [
                "llama.cpp AMD Radeon 780M Graphics qwen/qwen3-4b-gguf",
                "llama.cpp CPU qwen/qwen3-4b-gguf",
            ]
        );
        // An integrated GPU is not a card: presets still see no accelerator.
        assert_eq!(hardware.accelerator_state(), AcceleratorState::Absent);
        let intel = Accelerator {
            vendor: AcceleratorVendor::Intel,
            name: String::new(),
            pci_slot: "0000:00:02.0".into(),
            render_node: Some("renderD128".into()),
            kernel_driver: "xe".into(),
            integrated: true,
            dedicated_mib: 0,
            shared_mib: 31_287,
            compute: None,
            restart: None,
        };
        assert_eq!(intel.label(), "Intel graphics");
        assert_eq!(
            ranked(&catalog, &machine(vec![intel])),
            [
                "llama.cpp Intel graphics qwen/qwen3-4b-gguf",
                "llama.cpp CPU qwen/qwen3-4b-gguf",
            ]
        );
    }

    #[test]
    fn a_cpu_alone_is_offered_only_when_free_memory_and_threads_can_run_it() {
        let catalog = catalog();
        let roomy = HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 16_384,
            available_memory_mib: 12_000,
            free_storage_mib: 100 * 1024,
            accelerators: Vec::new(),
        };
        let found = catalog.configurations(&roomy);
        assert_eq!(
            ranked(&catalog, &roomy),
            ["llama.cpp CPU qwen/qwen3-4b-gguf"]
        );
        assert_eq!(
            found[0].speed.class,
            SpeedClass::Slow,
            "honest about being slow"
        );
        assert_eq!(found[0].speed.tokens_per_second, 10);
        // Enough RAM in total, but the desktop already holds most of it.
        let busy = HardwareSnapshot {
            available_memory_mib: 8_000,
            ..roomy.clone()
        };
        assert!(catalog.configurations(&busy).is_empty());
        let few_threads = HardwareSnapshot {
            cpu_threads: 2,
            ..roomy.clone()
        };
        assert!(catalog.configurations(&few_threads).is_empty());
        let full_disk = HardwareSnapshot {
            free_storage_mib: 1_024,
            ..roomy
        };
        assert!(catalog.configurations(&full_disk).is_empty());
    }

    #[test]
    fn a_card_waiting_for_a_restart_holds_the_decision_instead_of_falling_to_cpu() {
        let catalog = catalog();
        let pending = machine(vec![
            radeon_780m(),
            Accelerator {
                compute: None,
                dedicated_mib: 0,
                restart: Some(RestartReason::DriverLibraryMismatch),
                ..rtx_4070_mobile()
            },
        ]);
        assert_eq!(
            pending.accelerator_state(),
            AcceleratorState::RestartRequired {
                vendor: AcceleratorVendor::Nvidia,
                reason: RestartReason::DriverLibraryMismatch,
            }
        );
        assert!(catalog.configurations(&pending).is_empty());
        let driverless = machine(vec![Accelerator {
            kernel_driver: String::new(),
            compute: None,
            dedicated_mib: 0,
            ..rtx_4070_mobile()
        }]);
        assert_eq!(
            driverless.accelerator_state(),
            AcceleratorState::NeedsDriver {
                vendor: AcceleratorVendor::Nvidia
            }
        );
        assert_eq!(
            machine(Vec::new()).accelerator_state(),
            AcceleratorState::Absent
        );
    }

    #[test]
    fn an_eight_gib_card_reporting_firmware_reserved_memory_is_the_device_accelerator() {
        let card = |dedicated_mib| {
            machine(vec![Accelerator {
                dedicated_mib,
                ..rtx_4070_mobile()
            }])
            .accelerator_state()
        };
        assert_eq!(
            card(8_188),
            AcceleratorState::Usable {
                kind: RuntimeKind::NvidiaCuda,
                vram_mib: 8_188
            }
        );
        assert_eq!(card(6_144), AcceleratorState::Absent);
        let rocm = machine(vec![Accelerator {
            vendor: AcceleratorVendor::Amd,
            kernel_driver: "amdgpu".into(),
            dedicated_mib: 16_368,
            compute: Some("6.2".into()),
            ..rtx_4070_mobile()
        }]);
        assert_eq!(
            rocm.accelerator_state(),
            AcceleratorState::Usable {
                kind: RuntimeKind::AmdRocm,
                vram_mib: 16_368
            }
        );
        let vulkan_only = machine(vec![Accelerator {
            compute: None,
            ..rocm.accelerators[0].clone()
        }]);
        assert!(matches!(
            vulkan_only.accelerator_state(),
            AcceleratorState::Usable {
                kind: RuntimeKind::Vulkan,
                ..
            }
        ));
    }

    #[test]
    fn a_catalogue_pins_runtimes_by_digest_and_marks_models_by_the_engines_that_load_them() {
        let catalog = catalog();
        assert_eq!(catalog.validate(), Ok(()));

        let mut retagged = catalog.clone();
        retagged.runtimes[0].oci_image = "registry.example.invalid/tilo/vllm:latest".into();
        assert!(retagged.validate().is_err());
        let mut other_digest = catalog.clone();
        other_digest.runtimes[0].artifact.sha256 = "b".repeat(64);
        assert!(
            other_digest.validate().is_err(),
            "the image must name the signed manifest"
        );
        // llama.cpp does not run CUDA builds here, and vLLM loads no GGUF.
        let mut foreign_kind = catalog.clone();
        foreign_kind.runtimes[1]
            .kinds
            .insert(RuntimeKind::NvidiaCuda);
        assert!(foreign_kind.validate().is_err());
        let mut wrong_engine = catalog.clone();
        wrong_engine.models[1].engines = [RuntimeEngine::Vllm].into();
        assert!(wrong_engine.validate().is_err());
        let mut unmarked = catalog.clone();
        unmarked.models[1].engines.clear();
        assert!(unmarked.validate().is_err());
        // A model no runtime of the catalogue can run is refused.
        let mut orphan = catalog.clone();
        orphan.runtimes.truncate(1);
        assert!(orphan.validate().is_err());

        let ModelArtifact::HfSnapshot { files } = &catalog.models[0].artifact else {
            panic!("snapshot fixture");
        };
        for unsafe_path in ["../escape", "/etc/passwd", "a//b", "a/./b", ""] {
            let mut files = files.clone();
            files[1].path = unsafe_path.into();
            let snapshot = ModelArtifact::HfSnapshot { files };
            assert!(snapshot.validate().is_err(), "{unsafe_path:?}");
        }
        let mut duplicate = files.clone();
        duplicate[1].path = "config.json".into();
        assert!(
            ModelArtifact::HfSnapshot { files: duplicate }
                .validate()
                .is_err()
        );
        let without_config = ModelArtifact::HfSnapshot {
            files: vec![files[1].clone()],
        };
        assert!(without_config.validate().is_err());
    }

    #[test]
    fn a_snapshot_identity_is_its_file_listing_in_any_order() {
        let catalog = catalog();
        let ModelArtifact::HfSnapshot { files } = &catalog.models[0].artifact else {
            panic!("snapshot fixture");
        };
        let identity = catalog.models[0].artifact.identity();
        let reversed = ModelArtifact::HfSnapshot {
            files: files.iter().rev().cloned().collect(),
        };
        assert_eq!(reversed.identity(), identity);
        let mut changed = files.clone();
        changed[1].artifact.size_bytes = 101;
        assert_ne!(
            ModelArtifact::HfSnapshot { files: changed }.identity(),
            identity
        );
        assert_eq!(catalog.models[1].artifact.download_bytes(), 2_497_280_256);
        assert!(catalog.models[1].artifact.download_bytes() < 3 * GIB);
    }
}
