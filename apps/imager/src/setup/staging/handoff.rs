//! The Imager's side of the staging ESP handoff ([`staging_handoff_model`]): the
//! handoff for a plan, and the secret-free profile it may carry onto the
//! unencrypted staging ESP. The placeholder the exporter reserves for it is
//! in [`super::fat`].

use serde::{Deserialize, Serialize};
pub use staging_handoff_model::{HANDOFF_FILE, pad_json};
use staging_handoff_model::{HostOs, StagedIdentity};

use super::plan::Plan;

/// The shared staging contract, with an optional metadata-only migration binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Handoff {
    staging: staging_handoff_model::Handoff,
}

impl std::ops::Deref for Handoff {
    type Target = staging_handoff_model::Handoff;

    fn deref(&self) -> &Self::Target {
        &self.staging
    }
}

impl std::ops::DerefMut for Handoff {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.staging
    }
}

/// Keys of the installer's secret-free media profile
/// (`InstallProfile::to_media_profile_json`).
const PROFILE_KEYS: [&str; 12] = [
    "schema",
    "purpose",
    "hostname",
    "username",
    "full_name",
    "device_label",
    "locale",
    "keyboard",
    "timezone",
    "filesystem",
    "prepared_setup",
    "tpm_handoff",
];

/// Refuse anything but the secret-free media profile shape, so a request can
/// never smuggle a passphrase onto the unencrypted staging ESP.
pub fn validate_profile(
    profile: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for (key, value) in profile {
        if !PROFILE_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "profile field {key:?} is not allowed in the handoff"
            ));
        }
        let ok = match key.as_str() {
            "schema" => value.as_u64() == Some(1),
            "purpose" => value
                .as_str()
                .is_some_and(install_profile_model::valid_purpose),
            "device_label" => value.as_str().is_some_and(|label| {
                label.is_empty()
                    || (!label.trim().is_empty()
                        && label.len() <= 64
                        && !label.chars().any(char::is_control))
            }),
            "prepared_setup" => {
                serde_json::from_value::<crate::model::PreparedSetupExpectation>(value.clone())
                    .is_ok_and(|receipt| receipt.valid())
            }
            "tpm_handoff" => value
                .as_str()
                .is_some_and(|handoff| handoff.len() <= 64 * 1024),
            _ => value.as_str().is_some_and(|s| s.len() <= 256),
        };
        if !ok {
            return Err(format!("profile field {key:?} has an invalid value"));
        }
    }
    Ok(())
}

/// The secret-free handoff profile for an Imager install profile.
pub fn profile_from_install(
    profile: &crate::model::InstallProfile,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    profile.validate_media_profile()?;
    let bytes = profile.to_media_profile_json().map_err(|e| e.to_string())?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_profile(&map)?;
    Ok(map)
}

impl Handoff {
    /// Validated compact JSON padded with spaces to exactly `len` bytes.
    pub fn to_padded(&self, len: u64) -> Result<Vec<u8>, String> {
        self.validate()?;
        pad_json(&serde_json::to_vec(self).map_err(|e| e.to_string())?, len)
    }

    /// Parse placeholder bytes (JSON plus trailing whitespace).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > staging_handoff_model::MAX_BYTES {
            return Err("staged installation record is unexpectedly large".into());
        }
        let handoff: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        handoff.validate()?;
        Ok(handoff)
    }

    /// Shared staging consistency plus the secret-free profile and migration binding.
    pub fn validate(&self) -> Result<(), String> {
        self.staging.validate()?;
        validate_profile(&self.profile)?;
        if let Some(raw) = &self.migration {
            #[cfg(not(target_os = "linux"))]
            {
                let _ = raw;
                return Err("Linux migration cannot be resumed on this host".into());
            }
            #[cfg(target_os = "linux")]
            {
                let plan = crate::setup::migration_handoff::Plan::parse(
                    &serde_json::to_vec(raw).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let source = &plan.home.volume;
                if self.host_os != HostOs::Linux
                    || plan.operation_id != self.operation_id
                    || !source.disk_guid.eq_ignore_ascii_case(&self.disk_guid)
                    || source.disk_bytes != self.disk_size_bytes
                    || source
                        .partition_guid
                        .eq_ignore_ascii_case(&self.staging_esp.partuuid)
                    || source
                        .partition_guid
                        .eq_ignore_ascii_case(&self.staging_root.partuuid)
                {
                    return Err(
                        "migration source does not belong to this retained Linux installation"
                            .into(),
                    );
                }
                for (first, last) in [
                    (self.target_extent.first_lba, self.target_extent.last_lba),
                    (self.staging_esp.first_lba, self.staging_esp.last_lba),
                    (self.staging_root.first_lba, self.staging_root.last_lba),
                ] {
                    if first <= source.last_lba && last >= source.first_lba {
                        return Err("migration source overlaps installation space".into());
                    }
                }
            }
        }
        Ok(())
    }
}

/// The handoff for a plan whose partitions now carry `esp_guid` and
/// `root_guid`. `original_boot_order` comes from the helper's live firmware
/// read (the Windows probe basis has none).
pub fn for_plan(
    operation_id: &str,
    plan: &Plan,
    esp_guid: &str,
    root_guid: &str,
    owned_boot_entry: &str,
    original_boot_order: Vec<String>,
    profile: serde_json::Map<String, serde_json::Value>,
) -> Handoff {
    Handoff {
        staging: staging_handoff_model::Handoff {
            schema: staging_handoff_model::SCHEMA,
            operation_id: operation_id.into(),
            host_os: plan.basis.host_os(),
            disk_guid: plan.basis.disk.guid.to_ascii_lowercase(),
            disk_size_bytes: plan.basis.disk.size_bytes,
            staging_esp: StagedIdentity {
                partuuid: esp_guid.to_ascii_lowercase(),
                first_lba: plan.staging_esp.first_lba,
                last_lba: plan.staging_esp.last_lba,
            },
            staging_root: StagedIdentity {
                partuuid: root_guid.to_ascii_lowercase(),
                first_lba: plan.staging_root.first_lba,
                last_lba: plan.staging_root.last_lba,
            },
            target_extent: plan.target_extent,
            owned_boot_entry: owned_boot_entry.into(),
            original_boot_order,
            profile,
            migration: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Handoff {
        let basis = super::super::plan::tests::linux_basis(
            512,
            256 << 30,
            vec![super::super::platform::Partition {
                number: 1,
                guid: "22222222-2222-4222-8222-222222222222".into(),
                type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                first_lba: 2048,
                last_lba: 1048575,
            }],
        );
        let plan = super::super::plan::compute(
            &basis,
            super::super::plan::Requirements {
                staging_esp_bytes: 600 << 20,
                staging_root_bytes: 40 << 30,
                target_bytes: 64 << 30,
            },
        )
        .unwrap();
        let mut profile = serde_json::Map::new();
        profile.insert("schema".into(), 1.into());
        profile.insert("hostname".into(), "tilo".into());
        for_plan(
            "0a0b0c0d-0000-4000-8000-000000000001",
            &plan,
            plan.staging_esp.partition_guid.as_deref().unwrap(),
            plan.staging_root.partition_guid.as_deref().unwrap(),
            "0100",
            vec!["0001".into()],
            profile,
        )
    }

    /// Run the parser that detects and publishes the live handoff, rather
    /// than a reimplementation of its field rules.
    #[cfg(target_os = "linux")]
    fn parse_live(bytes: &[u8]) -> Result<serde_json::Value, String> {
        imager_backend::test_support::parse_live_handoff(bytes)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn migration_round_trips_through_shared_and_live_consumers_and_fails_closed() {
        let mut handoff = sample();
        let migration = serde_json::json!({
            "schema": 1,
            "operation_id": handoff.operation_id,
            "categories": {"home": true, "flatpak": false, "accounts": true},
            "home_policy": "access-read-only",
            "home": {
                "volume": {
                    "disk_guid": handoff.disk_guid,
                    "disk_bytes": handoff.disk_size_bytes,
                    "sector_bytes": 512,
                    "partition_guid": "22222222-2222-4222-8222-222222222222",
                    "first_lba": 2048, "last_lba": 1048575,
                    "filesystem_uuid": "33333333-3333-4333-8333-333333333333",
                    "filesystem": "ext4"
                },
                "relative": {"platform": "Unix", "units": b"home/person"},
                "inode": 1234, "original_uid": 1000, "subvolume": null
            }
        });
        handoff.migration = Some(serde_json::from_value(migration.clone()).unwrap());
        let bytes = handoff.to_padded(16384).unwrap();
        let shared = staging_handoff_model::Handoff::parse(&bytes).unwrap();
        assert_eq!(shared, handoff.staging);
        assert_eq!(Handoff::parse(&bytes).unwrap(), handoff);
        let decoded = serde_json::to_value(&shared).unwrap();
        assert_eq!(
            decoded["migration"], migration,
            "retain all source and consent data"
        );
        crate::setup::migration_handoff::Plan::parse(
            &serde_json::to_vec(&decoded["migration"]).unwrap(),
        )
        .unwrap();

        assert_eq!(parse_live(&bytes).unwrap(), decoded);

        for pointer in [
            "/unexpected",
            "/migration/unexpected",
            "/migration/home/unexpected",
            "/migration/home/volume/unexpected",
            "/migration/home/relative/unexpected",
        ] {
            let mut invalid = decoded.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            invalid
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), true.into());
            let invalid = serde_json::to_vec(&invalid).unwrap();
            assert!(Handoff::parse(&invalid).is_err(), "{pointer}");
            assert!(
                staging_handoff_model::Handoff::parse(&invalid).is_err(),
                "{pointer}"
            );
            assert!(parse_live(&invalid).is_err(), "{pointer}");
        }
        for (pointer, value) in [
            ("/host_os", serde_json::json!("windows")),
            (
                "/migration/operation_id",
                serde_json::json!("different-operation"),
            ),
            ("/migration/home/volume/disk_bytes", serde_json::json!(1)),
            (
                "/migration/home/volume/last_lba",
                serde_json::json!(handoff.target_extent.first_lba),
            ),
            (
                "/migration/home/volume/partition_guid",
                serde_json::json!(handoff.staging_esp.partuuid),
            ),
            (
                "/migration/home/relative/units",
                serde_json::json!(b"home/../person"),
            ),
        ] {
            let mut invalid = decoded.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            let invalid = serde_json::to_vec(&invalid).unwrap();
            assert!(Handoff::parse(&invalid).is_err(), "{pointer}");
            assert!(
                staging_handoff_model::Handoff::parse(&invalid).is_err(),
                "{pointer}"
            );
            assert!(parse_live(&invalid).is_err(), "{pointer}");
        }
    }

    #[test]
    fn shared_staging_contract_remains_the_wire_format() {
        let handoff = sample();
        let bytes = handoff.to_padded(4096).unwrap();
        assert_eq!(
            staging_handoff_model::Handoff::parse(&bytes).unwrap(),
            handoff.staging
        );
        let json = serde_json::to_string(&handoff).unwrap();
        let duplicated = json.replacen('{', "{\"schema\":1,", 1);
        assert!(Handoff::parse(duplicated.as_bytes()).is_err());
        assert!(Handoff::parse(&vec![b' '; staging_handoff_model::MAX_BYTES + 1]).is_err());

        let mut malformed = handoff.clone();
        malformed.operation_id = "not-a-guid".into();
        assert!(malformed.to_padded(4096).is_err());
        let mut secret = handoff;
        secret.profile.insert("password".into(), "secret".into());
        assert!(secret.to_padded(4096).is_err());
    }

    /// Handoff encoding, validation and profile mapping.
    #[test]
    fn handoff_round_trips_validates_and_maps_only_profile_keys() {
        // Phase `padded_handoff_round_trips_through_a_whitespace_tolerant_parser`.
        {
            let h = sample();
            let bytes = h.to_padded(4096).unwrap();
            assert_eq!(bytes.len(), 4096);
            assert!(bytes.ends_with(b"    "));
            assert_eq!(Handoff::parse(&bytes).unwrap(), h);
            assert!(h.to_padded(16).is_err());
        }

        // Phase `validation_rejects_inconsistent_extents_and_secrets`.
        {
            let mut h = sample();
            h.target_extent.last_lba += 1;
            assert!(h.validate().is_err());
            let mut h = sample();
            h.profile
                .insert("encryption_passphrase".into(), "hunter2".into());
            assert!(validate_profile(&h.profile).is_err());
            let mut h = sample();
            h.owned_boot_entry = "boot".into();
            assert!(h.validate().is_err());
            let mut h = sample();
            h.owned_boot_entry = "00ab".into();
            assert!(h.validate().is_err(), "uppercase hex only");
            let mut h = sample();
            h.owned_boot_entry = "0001".into();
            assert!(h.validate().is_err(), "never an entry already in BootOrder");
            let mut unknown = serde_json::to_value(sample()).unwrap();
            unknown["extra"] = 1.into();
            assert!(Handoff::parse(&serde_json::to_vec(&unknown).unwrap()).is_err());
        }

        // Phase `install_profile_maps_to_the_media_profile_keys_only`.
        {
            let mut profile = crate::model::InstallProfile::default();
            profile.user.username = "ada".into();
            profile.encryption_passphrase = "correct horse".into();
            let map = profile_from_install(&profile).unwrap();
            assert!(map.keys().all(|k| PROFILE_KEYS.contains(&k.as_str())));
            assert!(
                !serde_json::to_string(&map)
                    .unwrap()
                    .contains("correct horse")
            );
        }
    }
}
