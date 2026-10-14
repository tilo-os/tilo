//! The Imager's side of the staging ESP handoff ([`staging_handoff_model`]): the
//! handoff for a plan, and the secret-free profile it may carry onto the
//! unencrypted staging ESP. The placeholder the exporter reserves for it is
//! in [`super::fat`].

pub use staging_handoff_model::{HANDOFF_FILE, Handoff, StagedIdentity, pad_json};

use super::plan::Plan;

/// Keys of the installer's secret-free media profile
/// (`InstallProfile::to_media_profile_json`).
const PROFILE_KEYS: [&str; 8] = [
    "schema",
    "hostname",
    "username",
    "full_name",
    "locale",
    "keyboard",
    "timezone",
    "filesystem",
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Handoff {
        let basis = super::super::plan::tests::linux_basis(512, 256 << 30, Vec::new());
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
