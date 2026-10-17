//! Portable elevated-helper progress, refusal and journal protocol.

use serde::{Deserialize, Serialize};

pub const STAGING_HELPER_ARG: &str = "--tilo-staging-helper";
pub const REQUEST_SCHEMA: u32 = 1;

/// Operation IDs become file names; allow only a UUID-like identifier.
pub fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == b'-')
}

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

impl Step {
    /// `stage` steps in execution order.
    pub const STAGE: [Self; 9] = [
        Self::Shrink,
        Self::CreateEsp,
        Self::CreateRoot,
        Self::WriteEsp,
        Self::WriteRoot,
        Self::Verify,
        Self::Handoff,
        Self::BootEntry,
        Self::BootNext,
    ];
    /// `remove` steps in execution order.
    pub const REMOVE: [Self; 5] = [
        Self::RemoveBootNext,
        Self::RemoveBootEntry,
        Self::RemoveRoot,
        Self::RemoveEsp,
        Self::RestoreShrink,
    ];
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Stage,
    Arm,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentPaths {
    #[serde(rename = "staging-esp")]
    pub staging_esp: String,
    #[serde(rename = "staging-root")]
    pub staging_root: String,
}

/// One line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub step: Step,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Final line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub ok: bool,
    /// Stable refusal ID (`plan-stale`, `boot-next-busy`, `digest-mismatch`, ...).
    pub code: Option<String>,
    pub message: Option<String>,
}

/// Stable refusal IDs written to [`Outcome::code`].
pub mod codes {
    /// The request is malformed or inconsistent with itself.
    pub const REQUEST_INVALID: &str = "request-invalid";
    /// The release is not in this binary's catalog or its manifest differs.
    pub const RELEASE_UNKNOWN: &str = "release-unknown";
    pub const MANIFEST_MISMATCH: &str = "manifest-mismatch";
    /// The host no longer matches the reviewed plan's basis; re-review.
    pub const PLAN_STALE: &str = "plan-stale";
    /// A component's bytes do not match the manifest.
    pub const DIGEST_MISMATCH: &str = "digest-mismatch";
    /// The staging ESP image lacks the handoff/record placeholders.
    pub const ESP_PLACEHOLDER: &str = "esp-placeholder-missing";
    /// The staging ESP image's FAT sectors are smaller than the disk's.
    pub const SECTOR_SIZE: &str = "sector-size-unsupported";
    /// Another operation on this disk is unfinished.
    pub const PENDING_OPERATION: &str = "pending-operation";
    pub const SECURE_BOOT_ON: &str = "secure-boot-on";
    /// `BootNext` names an entry that is not ours.
    pub const BOOT_NEXT_BUSY: &str = "boot-next-busy";
    /// Our `Boot####` changed (or its number was taken) since we wrote it.
    pub const BOOT_ENTRY_MODIFIED: &str = "boot-entry-modified";
    /// A staged partition, shrink or firmware entry no longer holds.
    pub const STAGING_CHANGED: &str = "staging-changed";
    /// `arm` without a completed `stage`, or `remove` without a record.
    pub const NOT_STAGED: &str = "not-staged";
    pub const RECORD_FAILED: &str = "record-failed";
    pub const IO_FAILED: &str = "io-failed";
    /// The helper exited without writing an outcome.
    pub const HELPER_EXITED: &str = "helper-exited";
}

/// Why the helper stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
