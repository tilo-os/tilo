//! The `host-info` resource: the facts of this device's hardware and system, read from the
//! kernel and the OS image and shown in About.
//!
//! Nothing here changes the machine: it is one reading, and `refresh` reads it again.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const HOST_INFO: &str = "host-info";

/// What this device is: its name, its system image, its kernel and its hardware.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct HostInfo {
    /// The OS image's name, as `/etc/os-release` gives it.
    pub pretty_name: String,
    /// The kernel release.
    pub kernel: String,
    /// How long the machine has been up, in words: `3d 4h 12m`.
    pub uptime: String,
    /// The machine's name on the network.
    pub hostname: String,
    /// The processor architecture.
    pub arch: String,
    /// The processor's model, or the board's system on a chip.
    pub cpu: String,
    /// The memory it has, in words: `32 GB`.
    pub memory: String,
    /// The computer's model, as its firmware names it: `Framework Laptop 13 (AMD)`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The firmware's version, when it names one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub firmware: String,
    /// What the hardware check found, one entry per component class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<HostComponent>,
    /// A component the desktop needs is unsupported: Tilo cannot install here.
    #[serde(default)]
    pub blocked: bool,
    /// What this machine last reported as missing, when a fact could not be read.
    #[serde(default)]
    pub unknown: bool,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last read did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// One component class of this computer, as the hardware check found it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct HostComponent {
    /// The class: "graphics", "wifi", "storage".
    pub kind: String,
    /// The devices the check found in that class, as the system names them.
    #[serde(default)]
    pub name: String,
    /// "supported", "limited", "unsupported" or "unverified".
    pub state: String,
    /// Whether the check found such hardware: an absent optional part is no fault.
    #[serde(default)]
    pub present: bool,
}

/// The `host-info` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.host")]
pub enum HostInfoAction {
    /// Read the facts again: the uptime moves, and the system may have changed.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh system facts, systemdaten neu lesen"
    )]
    Refresh,
}
