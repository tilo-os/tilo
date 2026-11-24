//! The `windows-compat` resource: the Windows compatibility layer (Wine), whether it is installed
//! and what installing it takes, with the actions that install and remove it.
//!
//! Installing is a download of a disclosed size and needs the person's agreement; the layer is a
//! signed system extension installed by a privileged helper through polkit.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const WINDOWS_PROGRAMS: &str = "windows-compat";

/// How the compatibility layer stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum WindowsState {
    /// Installed and ready.
    #[default]
    Ready,
    /// Not installed; installing takes the person's agreement.
    NeedsInstall,
    Installing,
    /// Asked for while offline: it installs by itself once this device is online.
    WaitingForNetwork,
    Failed,
    /// This system cannot install it.
    NotOffered,
}

/// The step an install is taking, and the megabytes it has fetched so far.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct WindowsStep {
    /// What the step does, in the installer's words.
    #[serde(default)]
    pub label: String,
    /// Which step it is, counted from one, and how many there are.
    pub number: u32,
    pub count: u32,
    /// Megabytes fetched of the whole download, when its size is known.
    #[serde(default)]
    pub megabytes_done: Option<u64>,
    #[serde(default)]
    pub megabytes_total: Option<u64>,
}

/// An action the reading offers now: installing the layer, or removing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WindowsOffer {
    Install,
    Remove,
}

/// What this computer's Windows layer reads as now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct WindowsPrograms {
    /// Whether this system offers it at all.
    #[serde(default)]
    pub available: bool,
    pub state: WindowsState,
    /// The state in words, the size an install takes included.
    pub summary: String,
    /// How much space installing it takes, in words: `1.2 GB`.
    #[serde(default)]
    pub size: Option<String>,
    /// Why the last attempt failed, when it did.
    #[serde(default)]
    pub problem: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// How far an install in progress has got, 0 to 1; nothing unless one is running.
    #[serde(default)]
    pub progress: Option<f64>,
    /// The step an install in progress is taking, when the helper says which.
    #[serde(default)]
    pub step: Option<WindowsStep>,
    /// The actions offered now, in the order a card shows them.
    #[serde(default)]
    pub offered: Vec<WindowsOffer>,
}

/// The `windows-compat` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.windows-compat")]
pub enum WindowsProgramsAction {
    /// Download and install the compatibility layer, after the size is shown.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "install windows support, windows-unterstützung installieren"
    )]
    Install,
    /// Remove it; Windows apps and their disks are not touched.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove windows support, windows-unterstützung entfernen"
    )]
    Remove,
}
