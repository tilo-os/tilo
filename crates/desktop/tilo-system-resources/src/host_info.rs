//! The `host-info` resource: the facts of this device's hardware and system, read from the
//! kernel and the OS image and shown in About.
//!
//! Nothing here changes the machine: it is one reading, and `refresh` reads it again.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const HOST_INFO: &str = "host-info";

/// What this device is: its name, its system image, its kernel and its hardware.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
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
