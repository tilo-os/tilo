//! Durable host transaction record: every mutation is recorded `intended`
//! before it is attempted and `done` after it is read back.

use serde::{Deserialize, Serialize};

use super::plan::Plan;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Shrink,
    CreateEsp,
    CreateRoot,
    WriteEsp,
    WriteRoot,
    Verify,
    Handoff,
    BootEntry,
    BootNext,
    RemoveBootNext,
    RemoveBootEntry,
    RemoveRoot,
    RemoveEsp,
    RestoreShrink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepState {
    Intended,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub step: Step,
    pub state: StepState,
    #[serde(default)]
    pub detail: serde_json::Map<String, serde_json::Value>,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRef {
    pub release_id: String,
    pub manifest_sha256: String,
    pub architecture: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalFirmware {
    pub boot_order: Vec<String>,
    pub boot_next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedBootEntry {
    pub number: String,
    pub load_option_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: u32,
    pub operation_id: String,
    pub plan_sha256: String,
    pub plan: Plan,
    pub release: ReleaseRef,
    pub steps: Vec<StepRecord>,
    pub original_firmware: OriginalFirmware,
    pub owned_boot_entry: Option<OwnedBootEntry>,
}
