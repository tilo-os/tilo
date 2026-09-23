//! Pure placement: host facts + bundle sizes + chosen Tilo size → one reviewed
//! plan, or a refusal. No I/O. The helper recomputes `basis` from a fresh probe
//! and refuses when it differs (`plan-stale`).

use serde::{Deserialize, Serialize};

use super::platform::{Disk, SystemVolume};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareFacts {
    pub uefi: bool,
    pub secure_boot: bool,
    pub boot_current: Option<String>,
    pub boot_order: Vec<String>,
    pub boot_next: Option<String>,
}

/// Exactly the facts a plan was computed from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Basis {
    pub disk: Disk,
    pub system_volume: Option<SystemVolume>,
    pub firmware: FirmwareFacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shrink {
    pub partition_guid: String,
    pub from_bytes: u64,
    pub to_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedPartition {
    pub first_lba: u64,
    pub last_lba: u64,
    /// Pre-generated on Linux; `None` on Windows until creation records it.
    pub partition_guid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootEntry {
    pub description: String,
    /// Backslash-separated path inside the staging ESP.
    pub loader_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub basis: Basis,
    pub shrink: Option<Shrink>,
    pub target_extent: Extent,
    pub staging_esp: StagedPartition,
    pub staging_root: StagedPartition,
    pub boot_entry: BootEntry,
}

/// Bundle-derived sizes the plan must fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub staging_esp_bytes: u64,
    pub staging_root_bytes: u64,
    pub target_bytes: u64,
}

/// Bounds the Review stepper may offer for the Tilo size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetBounds {
    pub min_bytes: u64,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// Not enough space even at the maximum safe shrink; bytes short.
    InsufficientSpace {
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// The disk or partition map is outside what this planner supports.
    Unsupported(String),
}

impl Plan {
    /// Canonical JSON (serde field order, no whitespace) and its SHA-256.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).expect("plan serializes");
        format!("{:x}", Sha256::digest(bytes))
    }
}

/// Place staging + target on `basis`. Pure; see the design spec §2 for rules.
pub fn compute(basis: &Basis, requirements: Requirements) -> Result<Plan, PlanError> {
    let _ = (basis, requirements);
    Err(PlanError::Unsupported("planner not implemented".into()))
}

/// Tilo sizes the Review stepper may offer for `basis`, given staging sizes
/// (`requirements.target_bytes` is ignored).
pub fn target_bounds(basis: &Basis, requirements: Requirements) -> Result<TargetBounds, PlanError> {
    let _ = (basis, requirements);
    Err(PlanError::Unsupported("planner not implemented".into()))
}
