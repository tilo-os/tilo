//! The staging ESP handoff read by the live installer (`tilo-install
//! --internal-handoff`). A hint, never authority: the installer revalidates
//! every identity and extent and the person still confirms.

use serde::{Deserialize, Serialize};

use super::plan::Extent;

pub const HANDOFF_FILE: &str = "handoff.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIdentity {
    pub partuuid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub schema: u32,
    pub operation_id: String,
    /// `windows` or `linux`.
    pub host_os: String,
    pub disk_guid: String,
    pub disk_size_bytes: u64,
    pub staging_esp: StagedIdentity,
    pub staging_root: StagedIdentity,
    pub target_extent: Extent,
    /// Four uppercase hex digits, e.g. `0004`.
    pub owned_boot_entry: String,
    pub original_boot_order: Vec<String>,
    /// Secret-free fields accepted by the installer's media profile.
    pub profile: serde_json::Map<String, serde_json::Value>,
}
