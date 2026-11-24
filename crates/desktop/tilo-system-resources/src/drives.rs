//! The `drives` resource: the NTFS volumes this computer sees, the ones the person chose to
//! remember, and the actions that open, close, forget and configure them.
//!
//! What opens a drive is the NTFS mount service; it asks the person before opening a drive it
//! was not told to remember. This contract is the vocabulary a shell, Settings and an agent
//! speak to that service.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const DRIVES: &str = "drives";

/// How a drive is opened: the faster kernel driver, or a confined userspace one for drives the
/// person does not know.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriveMode {
    /// The faster driver.
    #[default]
    Open,
    /// A confined driver: a hostile file system reaches a sandbox, not the kernel.
    OpenSafely,
}

/// Where a drive sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Internal,
    /// USB, SD cards, Thunderbolt enclosures, a disk that appeared after start-up.
    External,
}

/// Why a drive opened read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyReason {
    /// Windows hibernated rather than shutting down.
    WindowsHibernated,
    /// Windows was not shut down.
    WindowsNotShutDown,
    /// The volume needs a check before it is written.
    NeedsCheck,
}

/// What the person may choose to remember about a drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RememberOffer {
    /// Nothing: it is asked about every time.
    #[default]
    Nothing,
    /// Only that it opens safely, and it is still asked about.
    OpenSafely,
    /// Either mode.
    AnyMode,
}

/// A drive that is open and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Mounted {
    /// Where it is open.
    pub path: String,
    pub mode: DriveMode,
    /// Why it is read-only, when it is.
    pub read_only: Option<ReadOnlyReason>,
}

/// What the person chose to remember about a drive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Remembered {
    /// The drive this choice is for, as the drive list names it.
    pub grant: String,
    pub mode: DriveMode,
}

/// An NTFS volume attached now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DriveVolume {
    /// Names the volume while it stays attached.
    pub id: String,
    pub label: String,
    pub size_bytes: u64,
    pub placement: Placement,
    /// Appeared after this computer started.
    pub hot_added: bool,
    /// The choice remembered for it, when one is.
    pub remembered: Option<Remembered>,
    /// Whether it needs the person's agreement before it opens.
    pub needs_consent: bool,
    /// The mode offered first.
    pub preselect: DriveMode,
    pub can_remember: RememberOffer,
    pub mounted: Option<Mounted>,
}

/// A drive the person chose to remember.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct RememberedDrive {
    /// The drive this choice is for.
    pub grant: String,
    pub label: String,
    pub placement: Placement,
    pub mode: DriveMode,
    /// Where it is open now, when it is.
    pub mounted: Option<String>,
}

/// What the computer's drives read as now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Drives {
    /// Whether the drive service answers here.
    #[serde(default)]
    pub serving: bool,
    /// Why the drives could not be read, until they can.
    #[serde(default)]
    pub read_error: Option<String>,
    /// The NTFS volumes attached now.
    pub drives: Vec<DriveVolume>,
    /// The drives the person chose to remember.
    pub remembered: Vec<RememberedDrive>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `drives` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.drives")]
pub enum DrivesAction {
    /// Read the drives and their remembered choices again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh drives, laufwerke neu lesen"
    )]
    Refresh,
    /// Open a drive, in the mode chosen, remembering the choice when asked to.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "drive",
        keywords = "open drive, mount, laufwerk öffnen"
    )]
    Mount {
        #[param("The drive, as the drive list names it")]
        drive: String,
        #[param("Whether to open it safely")]
        safely: bool,
        #[param("Whether to remember this choice for the drive")]
        remember: bool,
    },
    /// Close a drive that is open.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "x",
        keywords = "close drive, unmount, laufwerk schließen"
    )]
    Unmount {
        #[param("The drive, as the drive list names it")]
        drive: String,
    },
    /// Forget a remembered drive: it is asked about again the next time.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "forget drive, remember drive, laufwerk vergessen"
    )]
    Forget {
        #[param("The remembered drive, as the drive list names it")]
        grant: String,
    },
    /// Open a remembered drive safely or with the faster driver from its next opening on.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "check",
        keywords = "open safely, laufwerk sicher öffnen"
    )]
    SetMode {
        #[param("The remembered drive, as the drive list names it")]
        grant: String,
        #[param("Whether it opens safely")]
        safely: bool,
    },
}
