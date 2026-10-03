//! USB-free installation: the staged installer's handoff, as the UI sees it.
//!
//! Tilo Imager stages this live installer on the same disk as the retained
//! OS and leaves `/EFI/tilo-staging/handoff.json` on the staging ESP. Before
//! greetd starts, `tilo-staging-handoff detect` (root) publishes a validated
//! copy at [`HANDOFF_PATH`] only when the firmware loader really ran from the
//! ESP the handoff names. Field names mirror the Imager's
//! `setup::staging::handoff::Handoff` exactly (design spec §6).
//!
//! The handoff is a hint, never authority. The UI uses it to preselect the one
//! safe plan on the one disk it applies to and to prefill profile fields; the
//! privileged backend re-derives every fact before it writes anything.

use std::path::Path;

use serde::Deserialize;

pub const HANDOFF_PATH: &str = "/run/tilo-installer/handoff.json";
pub const HANDOFF_ERROR_PATH: &str = "/run/tilo-installer/handoff.error";
const MAX_HANDOFF_BYTES: usize = 64 * 1024;
/// LBAs beyond this are not a disk this installer can describe.
const MAX_LBA: u64 = 1 << 62;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIdentity {
    pub partuuid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
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

/// The operating system the person returns to when they cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

trait LbaRange {
    fn first(&self) -> u64;
    fn last(&self) -> u64;
    fn overlaps(&self, other: &dyn LbaRange) -> bool {
        self.first() <= other.last() && other.first() <= self.last()
    }
}

impl LbaRange for StagedIdentity {
    fn first(&self) -> u64 {
        self.first_lba
    }
    fn last(&self) -> u64 {
        self.last_lba
    }
}

impl LbaRange for Extent {
    fn first(&self) -> u64 {
        self.first_lba
    }
    fn last(&self) -> u64 {
        self.last_lba
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

fn is_boot_id(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

fn valid_range(first: u64, last: u64) -> bool {
    first > 0 && first <= last && last < MAX_LBA
}

impl Handoff {
    /// Structural validation shared by every consumer. It deliberately says
    /// nothing about the disk; see [`crate::disks::Disk::apply_internal_handoff`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason for the first violated rule.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err("unsupported staged installation version".into());
        }
        if !is_guid(&self.operation_id) || !is_guid(&self.disk_guid) {
            return Err("staged installation identity is malformed".into());
        }
        if !matches!(self.host_os.as_str(), "windows" | "linux") {
            return Err("staged installation names an unsupported host".into());
        }
        if self.disk_size_bytes == 0 {
            return Err("staged installation has no disk size".into());
        }
        for staged in [&self.staging_esp, &self.staging_root] {
            if !is_guid(&staged.partuuid) || !valid_range(staged.first_lba, staged.last_lba) {
                return Err("staged installer partition is malformed".into());
            }
        }
        if !valid_range(self.target_extent.first_lba, self.target_extent.last_lba) {
            return Err("staged target space is malformed".into());
        }
        if self
            .staging_esp
            .partuuid
            .eq_ignore_ascii_case(&self.staging_root.partuuid)
        {
            return Err("staged installer partitions share one identity".into());
        }
        if self.staging_esp.overlaps(&self.staging_root)
            || self.target_extent.overlaps(&self.staging_esp)
            || self.target_extent.overlaps(&self.staging_root)
        {
            return Err("staged target space overlaps the staged installer".into());
        }
        if !is_boot_id(&self.owned_boot_entry)
            || self.original_boot_order.len() > 128
            || !self.original_boot_order.iter().all(|id| is_boot_id(id))
            || self.original_boot_order.contains(&self.owned_boot_entry)
        {
            return Err("staged firmware state is malformed".into());
        }
        Ok(())
    }

    pub fn host(&self) -> HostOs {
        if self.host_os == "linux" {
            HostOs::Linux
        } else {
            HostOs::Windows
        }
    }

    /// The Imager-supplied setup choices, validated field by field exactly
    /// like a media profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile object carries unexpected fields or
    /// an unsupported schema.
    pub fn media_profile(&self) -> Result<crate::media_profile::MediaProfile, String> {
        crate::media_profile::validate_object(&self.profile)
    }
}

/// Parse and structurally validate handoff bytes.
///
/// # Errors
///
/// Returns an error for oversized input, malformed JSON, duplicate or unknown
/// fields, or any violated structural rule.
pub fn parse(bytes: &[u8]) -> Result<Handoff, String> {
    if bytes.len() > MAX_HANDOFF_BYTES {
        return Err("staged installation record is unexpectedly large".into());
    }
    let handoff = serde_json::from_slice::<Handoff>(bytes)
        .map_err(|error| format!("staged installation record is malformed: {error}"))?;
    handoff.validate()?;
    Ok(handoff)
}

/// Load the published copy. `Ok(None)` is the ordinary USB-media case.
///
/// # Errors
///
/// Returns an error when the copy exists but cannot be read or validated.
pub fn load_from(path: &Path) -> Result<Option<Handoff>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read the staged installation: {error}")),
    };
    parse(&bytes).map(Some)
}

/// Load the copy published at [`HANDOFF_PATH`].
///
/// # Errors
///
/// See [`load_from`].
pub fn load() -> Result<Option<Handoff>, String> {
    load_from(Path::new(HANDOFF_PATH))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const GIB_SECTORS: u64 = 1024 * 1024 * 1024 / 512;
    /// Matches `os/tests/staging_handoff_test.py`: target 64 GiB after a
    /// 100 GiB Windows region, then a 1 GiB staging ESP and 40 GiB root.
    pub const TARGET: (u64, u64) = (
        100 * GIB_SECTORS + 2048,
        100 * GIB_SECTORS + 2048 + 64 * GIB_SECTORS - 1,
    );
    pub const STAGING_ESP: (u64, u64) = (TARGET.1 + 1, TARGET.1 + GIB_SECTORS);
    pub const STAGING_ROOT: (u64, u64) = (STAGING_ESP.1 + 1, STAGING_ESP.1 + 40 * GIB_SECTORS);

    pub fn document() -> serde_json::Value {
        serde_json::json!({
            "schema": 1,
            "operation_id": "0f0e0d0c-0b0a-4908-8706-050403020100",
            "host_os": "windows",
            "disk_guid": "11111111-2222-4333-8444-555555555555",
            "disk_size_bytes": 256u64 * 1024 * 1024 * 1024,
            "staging_esp": {"partuuid": "aaaaaaaa-0000-4000-8000-000000000001",
                            "first_lba": STAGING_ESP.0, "last_lba": STAGING_ESP.1},
            "staging_root": {"partuuid": "bbbbbbbb-0000-4000-8000-000000000002",
                             "first_lba": STAGING_ROOT.0, "last_lba": STAGING_ROOT.1},
            "target_extent": {"first_lba": TARGET.0, "last_lba": TARGET.1},
            "owned_boot_entry": "0104",
            "original_boot_order": ["0001", "0000"],
            "profile": {"hostname": "studio", "username": "ada", "locale": "en_US.UTF-8",
                        "keyboard": "de"},
        })
    }

    /// `lsblk -J -b -p` output for the disk [`document`] describes, with the
    /// live root on the staging root partition.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixture geometry is a handful of small constants"
    )]
    pub fn lsblk_json() -> serde_json::Value {
        let part = |name: &str,
                    uuid: &str,
                    kind: &str,
                    (first, last): (u64, u64),
                    mounts: serde_json::Value| {
            serde_json::json!({"name": name, "type": "part", "start": first, "size": (last - first + 1) * 512,
                               "partuuid": uuid, "parttype": kind, "fstype": "", "mountpoints": mounts})
        };
        serde_json::json!({"blockdevices": [{
        "name": "/dev/nvme0n1", "size": 256u64 * 1024 * 1024 * 1024, "model": "NVMe", "type": "disk",
        "rm": false, "ro": false, "serial": "INTERNAL", "maj:min": "259:0", "pttype": "gpt",
        "ptuuid": "11111111-2222-4333-8444-555555555555", "log-sec": 512,
        "children": [
            part("/dev/nvme0n1p1", "00000000-0000-4000-8000-00000000000a",
                 crate::disks::EFI_SYSTEM_GUID, (2048, 206_847), serde_json::json!([null])),
            part("/dev/nvme0n1p2", "00000000-0000-4000-8000-00000000000b",
                 "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7", (206_848, 100 * GIB_SECTORS - 1),
                 serde_json::json!([null])),
            part("/dev/nvme0n1p3", "aaaaaaaa-0000-4000-8000-000000000001",
                 crate::disks::EFI_SYSTEM_GUID, STAGING_ESP, serde_json::json!([null])),
            part("/dev/nvme0n1p4", "bbbbbbbb-0000-4000-8000-000000000002",
                 crate::disks::TILO_STAGING_ROOT_GUID, STAGING_ROOT,
                 serde_json::json!(["/sysroot", "/var"])),
        ]}]})
    }

    pub fn handoff() -> Handoff {
        parse(&serde_json::to_vec(&document()).unwrap()).unwrap()
    }

    fn rejects(mutate: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value = document();
        mutate(&mut value);
        parse(&serde_json::to_vec(&value).unwrap()).unwrap_err()
    }

    #[test]
    fn internal_handoff_accepts_the_imager_contract_and_fails_closed() {
        // Phase `accepts_the_imager_contract_and_names_the_host`.
        {
            let handoff = handoff();
            assert_eq!(handoff.host(), HostOs::Windows);
            assert_eq!(handoff.host().display_name(), "Windows");
            let profile = handoff.media_profile().unwrap();
            assert_eq!(profile.username.as_deref(), Some("ada"));
            assert_eq!(profile.keyboard_display_name(), Some("German (QWERTZ)"));
        }
        // Phase `unknown_duplicate_and_mistyped_fields_fail_closed`.
        {
            assert!(rejects(|v| v["target_disk"] = "/dev/sda".into()).contains("unknown field"));
            assert!(rejects(|v| v["disk_size_bytes"] = true.into()).contains("malformed"));
            assert!(rejects(|v| v["disk_size_bytes"] = (-1).into()).contains("malformed"));
            assert!(rejects(|v| v["staging_esp"]["extra"] = 1.into()).contains("unknown field"));
            let text = serde_json::to_string(&document()).unwrap();
            let duplicated = format!("{}, \"schema\": 1}}", text.trim_end_matches('}'));
            assert!(parse(duplicated.as_bytes()).is_err());
            let oversized = format!("{text}{}", " ".repeat(MAX_HANDOFF_BYTES));
            assert!(parse(oversized.as_bytes()).unwrap_err().contains("large"));
        }
        // Phase `structural_rules_are_enforced`.
        {
            assert!(rejects(|v| v["schema"] = 2.into()).contains("version"));
            assert!(rejects(|v| v["host_os"] = "macos".into()).contains("host"));
            assert!(rejects(|v| v["disk_guid"] = "not-a-guid".into()).contains("identity"));
            assert!(rejects(|v| v["owned_boot_entry"] = "04".into()).contains("firmware"));
            assert!(rejects(|v| v["owned_boot_entry"] = "000a".into()).contains("firmware"));
            assert!(
                rejects(|v| v["original_boot_order"] = serde_json::json!(["0104"]))
                    .contains("firmware")
            );
            assert!(
                rejects(|v| v["target_extent"]["last_lba"] = STAGING_ESP.0.into())
                    .contains("overlaps")
            );
            assert!(
                rejects(|v| v["staging_root"]["partuuid"] =
                    "AAAAAAAA-0000-4000-8000-000000000001".into())
                .contains("share")
            );
            assert!(rejects(|v| v["staging_esp"]["first_lba"] = 0.into()).contains("malformed"));
        }
        // Phase `profile_outside_the_media_contract_is_rejected_but_bad_values_are_repairs`.
        {
            let mut value = document();
            value["profile"]["password"] = "hunter2".into();
            let handoff = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(handoff.media_profile().is_err());

            let mut value = document();
            value["profile"]["hostname"] = "bad host".into();
            let handoff = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            let profile = handoff.media_profile().unwrap();
            assert!(profile.hostname.is_none());
            assert!(
                profile
                    .repairs
                    .contains(&crate::media_profile::MediaProfileField::Hostname)
            );
        }
        // Phase `missing_copy_is_the_ordinary_usb_case`.
        {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(load_from(&dir.path().join("handoff.json")), Ok(None));
            std::fs::write(dir.path().join("handoff.json"), b"{").unwrap();
            assert!(load_from(&dir.path().join("handoff.json")).is_err());
        }
    }
}
