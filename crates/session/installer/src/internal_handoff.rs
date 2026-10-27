//! USB-free installation: the staged installer's handoff, as the UI sees it.
//!
//! Tilo Imager stages this live installer on the same disk as the retained
//! OS and leaves `/EFI/tilo-staging/handoff.json` on the staging ESP. Before
//! greetd starts, `tilo-staging-handoff detect` (root) publishes a validated
//! copy at [`HANDOFF_PATH`] only when the firmware loader really ran from the
//! ESP the handoff names. The Imager writes it with the same
//! [`staging_handoff_model`] types and rules.
//!
//! The handoff is a hint, never authority. The UI uses it to preselect the one
//! safe plan on the one disk it applies to and to prefill profile fields; the
//! privileged backend re-derives every fact before it writes anything.

use std::path::Path;

pub use staging_handoff_model::{Extent, Handoff, HostOs, StagedIdentity, is_guid};

pub const HANDOFF_PATH: &str = "/run/tilo-installer/handoff.json";
pub const HANDOFF_ERROR_PATH: &str = "/run/tilo-installer/handoff.error";

/// The Imager-supplied setup choices, validated field by field exactly like
/// a media profile.
pub fn media_profile(handoff: &Handoff) -> Result<crate::media_profile::MediaProfile, String> {
    crate::media_profile::validate_object(&handoff.profile)
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
    Handoff::parse(&bytes).map(Some)
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
    /// Matches `crates/system/install-helpers/tests/staging_handoff.rs`:
    /// target 64 GiB after a 100 GiB Windows region, then a 1 GiB staging ESP
    /// and 40 GiB root.
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
            "profile": {"hostname": "studio", "username": "quinten", "locale": "en_US.UTF-8",
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
        Handoff::parse(&serde_json::to_vec(&document()).unwrap()).unwrap()
    }

    fn rejects(mutate: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value = document();
        mutate(&mut value);
        Handoff::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err()
    }

    #[test]
    fn internal_handoff_accepts_the_imager_contract_and_fails_closed() {
        {
            let handoff = handoff();
            assert_eq!(handoff.host_os, HostOs::Windows);
            assert_eq!(handoff.host_os.display_name(), "Windows");
            let profile = media_profile(&handoff).unwrap();
            assert_eq!(profile.username.as_deref(), Some("quinten"));
            assert_eq!(profile.keyboard_display_name(), Some("German (QWERTZ)"));
        }
        {
            assert!(rejects(|v| v["target_disk"] = "/dev/sda".into()).contains("unknown field"));
            assert!(rejects(|v| v["disk_size_bytes"] = true.into()).contains("malformed"));
            assert!(rejects(|v| v["disk_size_bytes"] = (-1).into()).contains("malformed"));
            assert!(rejects(|v| v["staging_esp"]["extra"] = 1.into()).contains("unknown field"));
            let text = serde_json::to_string(&document()).unwrap();
            let duplicated = format!("{}, \"schema\": 1}}", text.trim_end_matches('}'));
            assert!(Handoff::parse(duplicated.as_bytes()).is_err());
            let oversized = format!("{text}{}", " ".repeat(staging_handoff_model::MAX_BYTES));
            assert!(
                Handoff::parse(oversized.as_bytes())
                    .unwrap_err()
                    .contains("large")
            );
        }
        {
            assert!(rejects(|v| v["schema"] = 2.into()).contains("version"));
            assert!(rejects(|v| v["host_os"] = "macos".into()).contains("unknown variant"));
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
        {
            // The handoff profile is the closed, secret-free media profile: a
            // credential is refused before the media contract is consulted.
            assert!(rejects(|v| v["profile"]["password"] = "hunter2".into()).contains("password"));

            // The handoff carries the profile's schema opaquely; refusing an
            // unsupported one is the media contract's decision.
            let mut value = document();
            value["profile"]["schema"] = 2.into();
            let handoff = Handoff::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(media_profile(&handoff).is_err());

            let mut value = document();
            value["profile"]["hostname"] = "bad host".into();
            let handoff = Handoff::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            let profile = media_profile(&handoff).unwrap();
            assert!(profile.hostname.is_none());
            assert!(
                profile
                    .repairs
                    .contains(&crate::media_profile::MediaProfileField::Hostname)
            );
        }
        {
            // The ordinary USB and older staged handoffs carry no sidecar and
            // keep parsing unchanged.
            assert!(media_profile(&handoff()).unwrap().tpm_handoff.is_none());

            let package = serde_json::json!({
                "schema": 1, "sealed_bytes": 48, "ciphertext_sha256": "a".repeat(64)
            });
            let mut value = document();
            value["profile"]["prepared_setup"] = package;
            value["profile"]["tpm_handoff"] = "sealed sidecar".into();
            let handoff = Handoff::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(
                media_profile(&handoff).unwrap().tpm_handoff.as_deref(),
                Some("sealed sidecar")
            );

            // A sidecar without the prepared package it belongs to is refused,
            // and so is a value that is not a bounded string.
            let mut value = document();
            value["profile"]["tpm_handoff"] = "sealed sidecar".into();
            let handoff = Handoff::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(media_profile(&handoff).is_err());
            assert!(rejects(|v| v["profile"]["tpm_handoff"] = 1.into()).contains("tpm_handoff"));
        }
        {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(load_from(&dir.path().join("handoff.json")), Ok(None));
            std::fs::write(dir.path().join("handoff.json"), b"{").unwrap();
            assert!(load_from(&dir.path().join("handoff.json")).is_err());
        }
    }
}
