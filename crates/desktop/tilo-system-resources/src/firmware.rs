//! The `firmware` resource: the firmware updates `fwupd` offers for this machine's devices, what
//! it installed before, and the actions that refresh the inventory and install what it offers.
//!
//! fwupd validates the signed metadata and firmware and owns authorisation; this contract is the
//! vocabulary a shell, Settings and an agent speak to it.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const FIRMWARE: &str = "firmware";

/// One firmware update `fwupd` offers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct FirmwareUpdate {
    /// fwupd's stable device id, which scopes an install to the inventory the person approved.
    #[serde(default)]
    pub device_id: Option<String>,
    pub device: String,
    #[serde(default)]
    pub current_version: Option<String>,
    #[serde(default)]
    pub new_version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub urgency: Option<String>,
    #[serde(default)]
    pub problems: Vec<String>,
}

/// One firmware install this machine made.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct FirmwareHistoryEntry {
    pub device: String,
    #[serde(default)]
    pub from_version: Option<String>,
    #[serde(default)]
    pub to_version: Option<String>,
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// What this computer's firmware reads as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Firmware {
    /// Whether fwupd answers on this machine.
    #[serde(default)]
    pub available: bool,
    /// The updates it offers.
    pub updates: Vec<FirmwareUpdate>,
    /// What it installed before, newest first.
    pub history: Vec<FirmwareHistoryEntry>,
    /// An install asked for here is running.
    #[serde(default)]
    pub installing: bool,
    /// Why the inventory or the last action failed, when it did.
    #[serde(default)]
    pub problem: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
}

/// The `firmware` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.firmware")]
pub enum FirmwareAction {
    /// Read fwupd's metadata again, then the updates it offers.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh firmware, firmware neu lesen"
    )]
    Refresh,
    /// Install every firmware update offered now.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "install firmware, firmware installieren"
    )]
    Install,
}
