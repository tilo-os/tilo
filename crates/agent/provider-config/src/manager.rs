//! The snapshot `tilo-model-manager` publishes and Settings renders: one
//! contract, owned here, so the service and its client cannot drift.
//!
//! The manager is the authority for what is installed and running on this
//! device. [`LocalModelSnapshot::runtime`] projects a snapshot into the
//! device-local [`LocalRuntime`] record the gateway resolves against.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::local::{
    Configuration, ExpectedSpeed, Fit, HardwareSnapshot, LocalEndpoint, LocalModelRelease,
    ModelArtifact, Placement, RuntimeEngine,
};
use crate::{LocalRuntime, ModelCapability, ModelDescriptor, ProviderProfile};

/// How well this device runs its recommended local model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareFit {
    Excellent,
    Good,
    Tight,
    /// A present accelerator becomes usable after a restart.
    RestartRequired,
    Unsupported,
}

impl From<Fit> for HardwareFit {
    fn from(fit: Fit) -> Self {
        match fit {
            Fit::Excellent => Self::Excellent,
            Fit::Good => Self::Good,
            Fit::Tight => Self::Tight,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareRecommendation {
    /// The fit of the recommended configuration.
    pub fit: HardwareFit,
    pub memory_gib: u16,
    pub free_storage_gib: Option<u16>,
    /// Why this is the recommendation, or why there is none.
    pub reason: String,
}

/// One signed catalogue release as the person sees it before anything is
/// downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelPackage {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    /// Signed capabilities for this exact revision. None implies another: a
    /// general image model never gains face recognition.
    #[serde(default)]
    pub capabilities: BTreeSet<ModelCapability>,
    /// Signed context window, in tokens.
    #[serde(default)]
    pub context_window: u32,
    /// Accelerator memory the model reserves while it runs, in MiB.
    #[serde(default)]
    pub accelerator_memory_mib: u64,
}

impl LocalModelPackage {
    /// The package a signed release is presented as.
    #[must_use]
    pub fn of(model: &LocalModelRelease) -> Self {
        Self {
            id: model.id.clone(),
            name: model.label.clone(),
            description: model.description.clone(),
            source: package_source(&model.artifact),
            revision: model.version.clone(),
            license: model.license_id.clone(),
            license_url: model.license_url.clone(),
            download_bytes: model.artifact.download_bytes(),
            installed_bytes: model.installed_size_bytes,
            capabilities: model.capabilities.clone(),
            context_window: model.context_window,
            accelerator_memory_mib: model.required_accelerator_mib,
        }
    }

    /// The provider-model view of this package.
    #[must_use]
    pub fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: self.id.clone(),
            label: self.name.clone(),
            context_window: (self.context_window > 0).then_some(self.context_window),
            revision: Some(self.revision.clone()),
            capabilities: self.capabilities.clone(),
            tool_calling: true,
            ..ModelDescriptor::default()
        }
    }
}

/// Where a release's bytes come from, as shown before download: the file for
/// a single object, the pinned revision's directory for a snapshot.
fn package_source(artifact: &ModelArtifact) -> String {
    match artifact {
        ModelArtifact::Gguf(object) => object.url.clone(),
        ModelArtifact::HfSnapshot { files } => files
            .iter()
            .find(|file| file.path == "config.json")
            .and_then(|file| file.artifact.url.strip_suffix("/config.json"))
            .unwrap_or_default()
            .to_owned(),
    }
}

/// One configuration that honestly runs on this device: a catalogue release,
/// the engine and processor it would run on here, and how it would run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableModel {
    pub package: LocalModelPackage,
    pub placement: Placement,
    pub engine: RuntimeEngine,
    /// The GPU as the person knows it; `None` on the CPU.
    pub device: Option<String>,
    /// Memory the model needs where it runs, in MiB.
    pub required_mib: u64,
    /// Memory it may take there without starving the desktop, in MiB.
    pub budget_mib: u64,
    /// Whether that memory is the RAM the desktop runs in.
    pub shared_memory: bool,
    pub fit: Fit,
    pub speed: ExpectedSpeed,
}

impl AvailableModel {
    /// A stable key for this configuration, for resource ids.
    #[must_use]
    pub fn key(&self) -> String {
        self.placement.key(&self.package.id)
    }
}

impl From<&Configuration<'_>> for AvailableModel {
    fn from(configuration: &Configuration<'_>) -> Self {
        Self {
            package: LocalModelPackage::of(configuration.model),
            placement: configuration.placement(),
            engine: configuration.runtime.engine,
            device: configuration.device.map(crate::local::Accelerator::label),
            required_mib: configuration.required_mib,
            budget_mib: configuration.budget_mib,
            shared_memory: configuration.shared_memory,
            fit: configuration.fit,
            speed: configuration.speed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LocalModelPhase {
    Downloading {
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Installing {
        percent: Option<u8>,
        message: String,
    },
    Ready {
        running: bool,
        /// Exact loopback endpoint published by the supervised model service.
        /// A missing endpoint is never substituted with a guessed port.
        #[serde(default)]
        endpoint: Option<LocalEndpoint>,
    },
    Error {
        message: String,
    },
}

impl LocalModelPhase {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Downloading { .. } | Self::Installing { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledLocalModel {
    pub package: LocalModelPackage,
    /// Where it was set up to run.
    pub placement: Placement,
    pub engine: RuntimeEngine,
    /// The GPU as the person knows it; `None` on the CPU.
    pub device: Option<String>,
    pub phase: LocalModelPhase,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AcceleratorSetupState {
    Available,
    Queued,
    Staging,
    RestartRequired,
    Active,
    Error { message: String },
}

impl AcceleratorSetupState {
    #[must_use]
    pub fn blocks_model_setup(&self) -> bool {
        !matches!(self, Self::Active)
    }

    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Queued | Self::Staging)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorSetup {
    pub boot_pack_id: String,
    pub status: AcceleratorSetupState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelSnapshot {
    pub hardware: HardwareRecommendation,
    /// The raw facts presets and per-task fit are derived from.
    #[serde(default)]
    pub facts: HardwareSnapshot,
    /// Every configuration that runs on this device, best first: the first
    /// one declaring what a task needs is its recommendation, the rest its
    /// alternatives.
    #[serde(default)]
    pub available: Vec<AvailableModel>,
    #[serde(default)]
    pub models: Vec<InstalledLocalModel>,
    #[serde(default)]
    pub accelerator_setup: Option<AcceleratorSetup>,
}

impl LocalModelSnapshot {
    /// The device-local runtime record for this snapshot: the hardware facts
    /// and, once any model is installed, the profile of the installed models.
    /// The profile is supervised and enabled only while the manager reports
    /// the running model's socket; installed bytes alone never produce a
    /// usable endpoint.
    #[must_use]
    pub fn runtime(&self) -> LocalRuntime {
        let ready: Vec<&InstalledLocalModel> = self
            .models
            .iter()
            .filter(|model| matches!(model.phase, LocalModelPhase::Ready { .. }))
            .collect();
        let running = ready.iter().find_map(|model| match &model.phase {
            LocalModelPhase::Ready {
                running: true,
                endpoint: Some(endpoint),
            } if endpoint.model_id == model.package.id && endpoint.socket.is_absolute() => {
                Some(endpoint)
            }
            _ => None,
        });
        let profile = (!ready.is_empty()).then(|| {
            // Recording the manager's socket is what makes the profile
            // supervised; nothing typed into a form can claim it.
            let mut profile =
                ProviderProfile::local_runtime(running.map(|endpoint| endpoint.socket.clone()));
            // One runtime process serves one model: only the running one is
            // offered while it runs, every installed one is listed otherwise.
            profile.models = ready
                .iter()
                .filter(|model| {
                    running.is_none_or(|endpoint| endpoint.model_id == model.package.id)
                })
                .map(|model| model.package.descriptor())
                .collect();
            profile.default_model = profile.models.first().map(|model| model.id.clone());
            profile
        });
        LocalRuntime {
            hardware: Some(self.facts.clone()),
            profile,
        }
    }

    /// Every configuration declaring every capability in `needs`, best
    /// first.
    pub fn matching<'a>(
        &'a self,
        needs: &[ModelCapability],
    ) -> impl Iterator<Item = &'a AvailableModel> + use<'a> {
        let needs = needs.to_vec();
        self.available.iter().filter(move |available| {
            needs
                .iter()
                .all(|capability| available.package.capabilities.contains(capability))
        })
    }

    /// The best configuration declaring every capability in `needs`.
    #[must_use]
    pub fn best_for(&self, needs: &[ModelCapability]) -> Option<&AvailableModel> {
        self.matching(needs).next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{RuntimeKind, SpeedClass};

    fn cpu() -> Placement {
        Placement {
            runtime: "llama-cpp".into(),
            kind: RuntimeKind::Cpu,
            device: None,
        }
    }

    fn available(package: LocalModelPackage, tokens_per_second: u16) -> AvailableModel {
        AvailableModel {
            package,
            placement: cpu(),
            engine: RuntimeEngine::LlamaCpp,
            device: None,
            required_mib: 4096,
            budget_mib: 16_384,
            shared_memory: true,
            fit: Fit::Excellent,
            speed: ExpectedSpeed {
                tokens_per_second,
                class: SpeedClass::Slow,
            },
        }
    }

    fn installed(package: LocalModelPackage, phase: LocalModelPhase) -> InstalledLocalModel {
        InstalledLocalModel {
            package,
            placement: cpu(),
            engine: RuntimeEngine::LlamaCpp,
            device: None,
            phase,
        }
    }

    fn package(id: &str, capabilities: &[ModelCapability]) -> LocalModelPackage {
        LocalModelPackage {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            source: String::new(),
            revision: "a".repeat(40),
            license: "Apache-2.0".into(),
            license_url: "https://example.test/license".into(),
            download_bytes: 1,
            installed_bytes: 1,
            capabilities: capabilities.iter().copied().collect(),
            context_window: 32_768,
            accelerator_memory_mib: 4096,
        }
    }

    fn snapshot(models: Vec<InstalledLocalModel>) -> LocalModelSnapshot {
        LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 32,
                free_storage_gib: None,
                reason: String::new(),
            },
            facts: HardwareSnapshot::default(),
            available: vec![
                available(package("vision", &[ModelCapability::Vision]), 9),
                available(package("reviewer", &[ModelCapability::TraceReview]), 12),
                available(
                    package("reviewer-large", &[ModelCapability::TraceReview]),
                    6,
                ),
            ],
            models,
            accelerator_setup: None,
        }
    }

    #[test]
    fn the_runtime_profile_uses_the_ready_manager_endpoint_only() {
        let running = installed(
            package("reviewer", &[ModelCapability::TraceReview]),
            LocalModelPhase::Ready {
                running: true,
                endpoint: Some(LocalEndpoint {
                    socket: "/run/user/1000/tilo/models/reviewer/api.sock".into(),
                    model_id: "reviewer".into(),
                }),
            },
        );
        let installed = installed(
            package("vision", &[ModelCapability::Vision]),
            LocalModelPhase::Ready {
                running: false,
                endpoint: None,
            },
        );
        let runtime = snapshot(vec![running, installed.clone()]).runtime();
        let profile = runtime.profile.expect("installed models publish a profile");
        assert!(profile.is_supervised());
        assert_eq!(profile.models.len(), 1);
        assert!(profile.models[0].declares(ModelCapability::TraceReview));
        assert_eq!(profile.models[0].context_window, Some(32_768));

        let stopped = snapshot(vec![installed]).runtime().profile.unwrap();
        assert!(!stopped.enabled);
        assert_eq!(stopped.models.len(), 1);
        assert!(snapshot(Vec::new()).runtime().profile.is_none());
    }

    #[test]
    fn a_task_finds_the_best_release_declaring_what_it_needs() {
        let snapshot = snapshot(Vec::new());
        assert_eq!(
            snapshot
                .best_for(&[ModelCapability::TraceReview])
                .map(|available| available.package.id.as_str()),
            Some("reviewer")
        );
        assert_eq!(
            snapshot
                .matching(&[ModelCapability::TraceReview])
                .map(|available| available.package.id.as_str())
                .collect::<Vec<_>>(),
            ["reviewer", "reviewer-large"],
            "the rest are the alternatives, in rank order"
        );
        assert!(
            snapshot
                .best_for(&[ModelCapability::FaceRecognition])
                .is_none()
        );
    }
}
