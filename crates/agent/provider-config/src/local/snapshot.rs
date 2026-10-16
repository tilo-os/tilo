//! What the model manager (`tilo-model-manager snapshot`) reports and
//! Settings shows: the machine's fit, the signed models and their install
//! state, and the accelerator boot pack. One definition for both sides, so
//! the JSON contract cannot drift.

use serde::{Deserialize, Serialize};

use super::LocalEndpoint;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareFit {
    Excellent,
    Good,
    Tight,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent signed declaration of the release"
)]
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
    /// Signed capability of this exact revision, never inferred from a
    /// model name.
    #[serde(default)]
    pub vision: bool,
    /// Signed biometric capability for a distinct local face runtime. A
    /// general image-analysis model does not gain it implicitly.
    #[serde(default)]
    pub face_recognition: bool,
    /// The model runs inside the apps that use it: it is installed, never
    /// started, and is not a chat model for the local provider profile.
    #[serde(default)]
    pub in_process: bool,
    /// Signed salient-object segmentation capability ("Select subject").
    #[serde(default)]
    pub segmentation: bool,
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
    /// Whether the phase moves on by itself: a download or an install.
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

/// A signed model that runs in-process, offered whether or not it is
/// installed. `phase` is absent until it is set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InProcessLocalModel {
    pub package: LocalModelPackage,
    #[serde(default)]
    pub phase: Option<LocalModelPhase>,
}

impl InProcessLocalModel {
    /// Whether setting it up (again) is what is offered.
    #[must_use]
    pub fn installable(&self) -> bool {
        matches!(self.phase, None | Some(LocalModelPhase::Error { .. }))
    }

    /// Whether there are local bytes or a failed record to remove.
    #[must_use]
    pub fn removable(&self) -> bool {
        matches!(
            self.phase,
            Some(LocalModelPhase::Ready { .. } | LocalModelPhase::Error { .. })
        )
    }
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
    /// Whether models wait for the accelerator before they can be set up.
    #[must_use]
    pub fn blocks_model_setup(&self) -> bool {
        !matches!(self, Self::Active)
    }

    /// Whether the setup moves on by itself.
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
    pub recommendation: Option<LocalModelPackage>,
    /// Installed runtime-served models.
    #[serde(default)]
    pub models: Vec<InstalledLocalModel>,
    /// Every signed in-process model, installed or not.
    #[serde(default)]
    pub in_process: Vec<InProcessLocalModel>,
    #[serde(default)]
    pub accelerator_setup: Option<AcceleratorSetup>,
}

impl LocalModelSnapshot {
    /// The in-process model `id`.
    #[must_use]
    pub fn in_process_model(&self, id: &str) -> Option<&InProcessLocalModel> {
        self.in_process.iter().find(|model| model.package.id == id)
    }
}
