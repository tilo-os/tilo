//! The `scanners` resource: the document scanners this computer can reach, with the action
//! that looks for them again.
//!
//! Scanning a page is the capture studio's work; this is the vocabulary Settings and an agent
//! read, so a person sees which scanners the machine found. What talks to SANE is the session's
//! system service.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const SCANNERS: &str = "scanners";

/// One document scanner this computer can reach.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct ScannerShown {
    /// The device, as its driver names it (`epson2:libusb:001:004`).
    pub device: String,
    /// What it is called, as the driver describes it.
    pub description: String,
}

/// What the computer's scanners read as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct Scanners {
    /// Whether a scanner service answers here at all: SANE on this machine.
    pub available: bool,
    /// The scanners it found.
    pub found: Vec<ScannerShown>,
    /// Actions taken and not landed yet, by the action's id.
    pub pending: Vec<String>,
    /// Why the last look did not happen, until the next one.
    pub problem: Option<String>,
}

/// The `scanners` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.scanners")]
pub enum ScanAction {
    /// Look for scanners on this machine again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        idempotent,
        icon = "scan-line",
        keywords = "find scanner, look for scanners, scanner suchen"
    )]
    FindScanners,
}
