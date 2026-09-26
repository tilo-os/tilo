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

/// The inference engine a runtime image carries. The model manager builds
/// the engine's fixed argv from signed catalogue fields; the catalogue never
/// carries a command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEngine {
    /// vLLM's OpenAI-compatible server.
    #[default]
    Vllm,
}

/// A pinned runtime: an OCI image in its vendor's registry, pulled by digest.
///
/// `artifact` pins the image manifest itself (its registry URL, SHA-256 and
/// length), so `oci_image` names exactly that digest and the signed extension
/// record covers the same bytes the container engine verifies on pull. The
/// image is never mirrored or repackaged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRelease {
    pub id: String,
    pub kind: RuntimeKind,
    pub version: String,
    #[serde(default)]
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
    pub allowed_runtimes: BTreeSet<RuntimeKind>,
    pub required_memory_mib: u64,
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
            if runtime.image_digest() != Some(runtime.artifact.sha256.as_str()) {
                return Err(ProvisionError::InvalidCatalog(
                    "runtime OCI image must be pinned to its signed manifest digest",
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
                    && model.installed_size_bytes / (1024 * 1024) <= hardware.free_storage_mib
            })
            .max_by_key(|model| {
                (
                    model.context_window,
                    Reverse(model.artifact.download_bytes()),
                )
            })
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
/// Smallest accelerator class, in GiB, a GPU runtime is selected for.
const MIN_ACCELERATOR_GIB: u64 = 8;

/// The memory class a card is sold as. Drivers report total VRAM minus what
/// the firmware reserves, so an 8 GiB card reads as e.g. 8188 MiB; a raw
/// `>= 8192` comparison would turn every such card away.
fn accelerator_class_gib(vram_mib: u64) -> u64 {
    vram_mib.saturating_add(512) / 1024
}

pub fn select_runtime(hardware: &HardwareSnapshot) -> RuntimeDecision {
    if hardware.accelerators.iter().any(|accelerator| matches!(accelerator, Accelerator::Nvidia { vram_mib, driver } if accelerator_class_gib(*vram_mib) >= MIN_ACCELERATOR_GIB && !driver.trim().is_empty())) {
        return RuntimeDecision::Ready(RuntimeKind::NvidiaCuda);
    }
    if hardware.accelerators.iter().any(|accelerator| matches!(accelerator, Accelerator::Amd { vram_mib, rocm: Some(version) } if accelerator_class_gib(*vram_mib) >= MIN_ACCELERATOR_GIB && !version.trim().is_empty())) {
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
                engine: RuntimeEngine::Vllm,
                artifact: artifact("runtime"),
                oci_image: "registry.example.invalid/tilo/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            }],
            models: vec![LocalModelRelease {
                id: "tiny-q4".into(),
                label: "Tiny Q4".into(),
                description: "A tiny test model".into(),
                version: "1".into(),
                artifact: ModelArtifact::HfSnapshot {
                    files: vec![
                        SnapshotFile {
                            path: "config.json".into(),
                            artifact: artifact("config.json"),
                        },
                        SnapshotFile {
                            path: "model.safetensors".into(),
                            artifact: artifact("model.safetensors"),
                        },
                    ],
                },
                license_id: "Apache-2.0".into(),
                license_url: "https://models.example.invalid/LICENSE".into(),
                requires_acceptance: true,
                installed_size_bytes: 200,
                allowed_runtimes: [RuntimeKind::Cpu].into(),
                required_memory_mib: 8192,
                required_accelerator_mib: 0,
                context_window: 8192,
                capabilities: BTreeSet::new(),
                accelerator_memory_percent: None,
            }],
        }
    }
    #[test]
    fn a_catalogue_pins_runtimes_by_manifest_digest_and_snapshots_by_file() {
        let mut catalog = catalog();
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

        let ModelArtifact::HfSnapshot { files } = &mut catalog.models[0].artifact else {
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
        assert_eq!(catalog.models[0].artifact.download_bytes(), 200);
    }

    fn cpu() -> HardwareSnapshot {
        HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 16384,
            free_storage_mib: 100,
            accelerators: vec![],
        }
    }

    #[test]
    fn picks_cuda_then_rocm_then_cpu_only_when_facts_support_it() {
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

    #[test]
    fn an_eight_gib_card_reporting_firmware_reserved_memory_still_selects_cuda() {
        let card = |vram_mib| HardwareSnapshot {
            accelerators: vec![Accelerator::Nvidia {
                vram_mib,
                driver: "580".into(),
            }],
            ..cpu()
        };
        assert_eq!(
            select_runtime(&card(8188)),
            RuntimeDecision::Ready(RuntimeKind::NvidiaCuda)
        );
        assert_eq!(
            select_runtime(&card(6144)),
            RuntimeDecision::Ready(RuntimeKind::Cpu)
        );
    }
}
