//! The snapshot `tilo-model-manager` publishes and Settings renders: one
//! contract, owned here, so the service and its client cannot drift.
//!
//! The manager is the authority for what is installed and running on this
//! device. [`LocalModelSnapshot::runtime`] projects a snapshot into the
//! device-local [`LocalRuntime`] record the gateway resolves against.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::local::{ExpectedSpeed, HardwareSnapshot, LocalEndpoint, RuntimeKind};
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareRecommendation {
    pub fit: HardwareFit,
    pub memory_gib: u16,
    pub accelerator: String,
    pub accelerator_memory_gib: Option<u16>,
    pub free_storage_gib: Option<u16>,
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

/// A catalogue release that honestly fits this device, with how it would
/// run here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableModel {
    pub package: LocalModelPackage,
    pub runtime: RuntimeKind,
    pub speed: ExpectedSpeed,
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
    pub recommendation: Option<LocalModelPackage>,
    /// Every signed release that fits this device, best first.
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
            // One vLLM process serves one model: only the running one is
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

    /// The best available release declaring every capability in `needs`.
    #[must_use]
    pub fn best_for(&self, needs: &[ModelCapability]) -> Option<&AvailableModel> {
        self.available.iter().find(|available| {
            needs
                .iter()
                .all(|capability| available.package.capabilities.contains(capability))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::SpeedClass;

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
                accelerator: "CPU".into(),
                accelerator_memory_gib: None,
                free_storage_gib: None,
                reason: String::new(),
            },
            facts: HardwareSnapshot::default(),
            recommendation: None,
            available: vec![
                AvailableModel {
                    package: package("vision", &[ModelCapability::Vision]),
                    runtime: RuntimeKind::Cpu,
                    speed: ExpectedSpeed {
                        tokens_per_second: 9,
                        class: SpeedClass::Slow,
                    },
                },
                AvailableModel {
                    package: package("reviewer", &[ModelCapability::TraceReview]),
                    runtime: RuntimeKind::Cpu,
                    speed: ExpectedSpeed {
                        tokens_per_second: 12,
                        class: SpeedClass::Slow,
                    },
                },
            ],
            models,
            accelerator_setup: None,
        }
    }

    #[test]
    fn the_runtime_profile_uses_the_ready_manager_endpoint_only() {
        let running = InstalledLocalModel {
            package: package("reviewer", &[ModelCapability::TraceReview]),
            phase: LocalModelPhase::Ready {
                running: true,
                endpoint: Some(LocalEndpoint {
                    socket: "/run/user/1000/tilo/models/reviewer/api.sock".into(),
                    model_id: "reviewer".into(),
                }),
            },
        };
        let installed = InstalledLocalModel {
            package: package("vision", &[ModelCapability::Vision]),
            phase: LocalModelPhase::Ready {
                running: false,
                endpoint: None,
            },
        };
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
        assert!(
            snapshot
                .best_for(&[ModelCapability::FaceRecognition])
                .is_none()
        );
    }
}
