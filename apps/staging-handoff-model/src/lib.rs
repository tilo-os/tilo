//! The staging ESP handoff: what Tilo Imager leaves at
//! `/EFI/tilo-staging/handoff.json` for the live installer it staged beside
//! the retained OS. Both sides use these types and rules, so the Imager never
//! writes a handoff the installer would refuse.
//!
//! On the ESP the file is a fixed-size placeholder: compact JSON followed by
//! ASCII spaces up to the file's size. Any JSON parser that permits trailing
//! whitespace (e.g. `serde_json::from_slice`, Python `json.loads`) reads it
//! unchanged.
//!
//! The handoff is a hint, never authority. The installer uses it to preselect
//! the one safe plan on the one disk it applies to and to prefill profile
//! fields, then re-derives every fact from the live disk before it writes.

use serde::{Deserialize, Serialize};

/// The handoff's file name in the staging ESP's `/EFI/tilo-staging`.
pub const HANDOFF_FILE: &str = "handoff.json";
pub const SCHEMA: u32 = 1;
/// The largest handoff a reader accepts.
pub const MAX_BYTES: usize = 64 * 1024;
/// LBAs beyond this are not a disk the installer can describe.
const MAX_LBA: u64 = 1 << 62;
/// The most firmware boot entries a handoff may name.
const MAX_BOOT_ORDER: usize = 128;

/// A partition the Imager staged, by identity and extent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIdentity {
    pub partuuid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// An inclusive range of logical blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub schema: u32,
    pub operation_id: String,
    pub host_os: HostOs,
    pub disk_guid: String,
    pub disk_size_bytes: u64,
    pub staging_esp: StagedIdentity,
    pub staging_root: StagedIdentity,
    /// The space the installer installs into, directly before the staging ESP.
    pub target_extent: Extent,
    /// Four uppercase hex digits, e.g. `0004`.
    pub owned_boot_entry: String,
    pub original_boot_order: Vec<String>,
    /// Secret-free fields accepted by the installer's media profile.
    pub profile: serde_json::Map<String, serde_json::Value>,
}

/// The operating system the Imager ran on, which the person returns to when
/// they cancel the installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    Windows,
    Linux,
}

impl HostOs {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
        }
    }
}

impl StagedIdentity {
    const fn extent(&self) -> Extent {
        Extent {
            first_lba: self.first_lba,
            last_lba: self.last_lba,
        }
    }
}

impl Extent {
    /// How many blocks the range spans.
    pub const fn lbas(self) -> u64 {
        self.last_lba
            .saturating_sub(self.first_lba)
            .saturating_add(1)
    }

    fn valid(self) -> bool {
        self.first_lba > 0 && self.first_lba <= self.last_lba && self.last_lba < MAX_LBA
    }

    const fn overlaps(self, other: Self) -> bool {
        self.first_lba <= other.last_lba && other.first_lba <= self.last_lba
    }

    /// Whether `next` starts on the block after this one ends.
    fn precedes(self, next: Self) -> bool {
        self.last_lba.checked_add(1) == Some(next.first_lba)
    }
}

/// Canonical 8-4-4-4-12 hexadecimal GUID text, in either case.
pub fn is_guid(value: &str) -> bool {
    let groups = value.split('-').collect::<Vec<_>>();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A firmware boot entry id: four uppercase hex digits.
fn is_boot_id(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

impl Handoff {
    /// The handoff's own consistency. It deliberately says nothing about the
    /// disk, which only the installer can see, nor about the profile, whose
    /// fields each side checks against the media profile rules.
    ///
    /// # Errors
    ///
    /// A human-readable reason for the first violated rule.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err("unsupported staged installation version".into());
        }
        if !is_guid(&self.operation_id) || !is_guid(&self.disk_guid) {
            return Err("staged installation identity is malformed".into());
        }
        if self.disk_size_bytes == 0 {
            return Err("staged installation has no disk size".into());
        }
        for staged in [&self.staging_esp, &self.staging_root] {
            if !is_guid(&staged.partuuid) || !staged.extent().valid() {
                return Err("staged installer partition is malformed".into());
            }
        }
        if !self.target_extent.valid() {
            return Err("staged target space is malformed".into());
        }
        if self
            .staging_esp
            .partuuid
            .eq_ignore_ascii_case(&self.staging_root.partuuid)
        {
            return Err("staged installer partitions share one identity".into());
        }
        let (target, esp, root) = (
            self.target_extent,
            self.staging_esp.extent(),
            self.staging_root.extent(),
        );
        if esp.overlaps(root) || target.overlaps(esp) || target.overlaps(root) {
            return Err("staged target space overlaps the staged installer".into());
        }
        if !target.precedes(esp) || !esp.precedes(root) {
            return Err("staged target space and installer are not contiguous".into());
        }
        if !is_boot_id(&self.owned_boot_entry)
            || self.original_boot_order.len() > MAX_BOOT_ORDER
            || !self.original_boot_order.iter().all(|id| is_boot_id(id))
            || self.original_boot_order.contains(&self.owned_boot_entry)
        {
            return Err("staged firmware state is malformed".into());
        }
        Ok(())
    }

    /// Parse and validate handoff bytes (JSON plus trailing whitespace).
    ///
    /// # Errors
    ///
    /// Oversized input, malformed JSON, duplicate or unknown fields, or any
    /// rule [`Self::validate`] checks.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("staged installation record is unexpectedly large".into());
        }
        let handoff = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| format!("staged installation record is malformed: {error}"))?;
        handoff.validate()?;
        Ok(handoff)
    }

    /// Validated compact JSON padded with spaces to exactly `len` bytes.
    ///
    /// # Errors
    ///
    /// Any rule [`Self::validate`] checks, or JSON that does not fit `len`.
    pub fn to_padded(&self, len: u64) -> Result<Vec<u8>, String> {
        self.validate()?;
        pad_json(&serde_json::to_vec(self).map_err(|e| e.to_string())?, len)
    }
}

/// `json` followed by ASCII spaces to exactly `len` bytes.
///
/// # Errors
///
/// When `json` is longer than `len`.
pub fn pad_json(json: &[u8], len: u64) -> Result<Vec<u8>, String> {
    let len = usize::try_from(len).map_err(|e| e.to_string())?;
    if json.len() > len {
        return Err(format!(
            "{} bytes of JSON do not fit the {len}-byte ESP placeholder",
            json.len()
        ));
    }
    let mut out = json.to_vec();
    out.resize(len, b' ');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff() -> Handoff {
        Handoff {
            schema: SCHEMA,
            operation_id: "0f0e0d0c-0b0a-4908-8706-050403020100".into(),
            host_os: HostOs::Linux,
            disk_guid: "11111111-2222-4333-8444-555555555555".into(),
            disk_size_bytes: 1 << 40,
            staging_esp: StagedIdentity {
                partuuid: "aaaaaaaa-0000-4000-8000-000000000001".into(),
                first_lba: 3000,
                last_lba: 3999,
            },
            staging_root: StagedIdentity {
                partuuid: "bbbbbbbb-0000-4000-8000-000000000002".into(),
                first_lba: 4000,
                last_lba: 9999,
            },
            target_extent: Extent {
                first_lba: 2048,
                last_lba: 2999,
            },
            owned_boot_entry: "0104".into(),
            original_boot_order: vec!["0001".into()],
            profile: serde_json::Map::new(),
        }
    }

    #[test]
    fn a_written_handoff_reads_back_and_a_gap_or_bad_entry_is_refused_on_both_sides() {
        let written = handoff().to_padded(4096).unwrap();
        assert_eq!(written.len(), 4096);
        assert_eq!(Handoff::parse(&written).unwrap(), handoff());
        assert!(handoff().to_padded(16).is_err());

        let mut gap = handoff();
        gap.target_extent.last_lba -= 1;
        assert!(gap.validate().unwrap_err().contains("contiguous"));
        assert!(gap.to_padded(4096).is_err(), "the writer refuses it too");

        let mut lowercase = handoff();
        lowercase.owned_boot_entry = "00ab".into();
        assert!(lowercase.validate().unwrap_err().contains("firmware"));
    }
}
