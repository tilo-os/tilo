//! The `disk-unlock` resource: the encrypted volumes enrolled for sibling unlock, how each keeps
//! the half of its key the person's other devices release, and which Wi-Fi networks early boot
//! may join to ask them.
//!
//! What reads and changes the enrolment is `tilo-auth`, through polkit: reading it is granted
//! without a prompt to an administrator at this machine's seat, and changing it asks for
//! authentication. This contract is the vocabulary a shell, Settings and an agent speak to it.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const DISK_UNLOCK: &str = "disk-unlock";

/// How a volume keeps the half of its key its siblings release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Sealing {
    /// Sealed to the machine's TPM: a disk taken to another machine cannot ask with it.
    Tpm2,
    /// Kept in the clear: anyone holding this disk can read it.
    Cleartext,
}

/// An encrypted volume enrolled for sibling unlock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct UnlockVolume {
    /// The volume's LUKS UUID.
    pub volume: String,
    /// Its device path, `/dev/disk/by-uuid/<volume>`.
    pub device: String,
    /// The keyslot the sibling half was enrolled into.
    pub keyslot: u32,
    pub sealing: Sealing,
    /// The Wi-Fi networks early boot may join to ask the person's other devices.
    #[serde(default)]
    pub networks: Vec<String>,
    /// The person's other devices that may release the half, as they are named here.
    #[serde(default)]
    pub siblings: Vec<String>,
}

/// What this computer's enrolled volumes read as now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DiskUnlock {
    /// The volumes enrolled for sibling unlock.
    pub volumes: Vec<UnlockVolume>,
    /// The Wi-Fi connections saved on this computer, which a volume may use at boot.
    pub saved_networks: Vec<String>,
    /// Whether the enrolment has been read since the service started.
    #[serde(default)]
    pub scanned: bool,
    /// Why the enrolment could not be read, when it could not.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `disk-unlock` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.disk-unlock")]
pub enum DiskUnlockAction {
    /// Read the enrolled volumes again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh unlocking, entsperrung neu lesen"
    )]
    Refresh,
    /// Choose the whole list of Wi-Fi connections a volume may use at boot: the complete new
    /// list, so a replay asks for one definite state.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "check",
        keywords = "unlock network, entsperrungsnetzwerk"
    )]
    SetNetworks {
        #[param("The enrolled volume's LUKS UUID")]
        volume: String,
        #[param("The complete list of saved Wi-Fi connection names")]
        networks: Vec<String>,
    },
}
