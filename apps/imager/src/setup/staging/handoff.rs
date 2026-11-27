//! The Imager's side of the staging ESP handoff: the handoff for a plan, and the
//! secret-free profile it may carry.

use serde::{Deserialize, Serialize};
use staging_handoff_model::StagedIdentity;
pub use staging_handoff_model::{HANDOFF_FILE, pad_json};

use crate::backend::plan::Plan;

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

const PROFILE_KEYS: [&str; 10] = [
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
];

fn validate_profile_with(
    profile: &serde_json::Map<String, serde_json::Value>,
    strict_hostname: bool,
) -> Result<(), String> {
    for (key, value) in profile {
        if !PROFILE_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "profile field {key:?} is not allowed in the handoff"
            ));
        }
        let ok = match key.as_str() {
            "schema" => value.as_u64() == Some(1),
            "hostname" if strict_hostname => value
                .as_str()
                .is_some_and(staging_handoff_model::valid_hostname),
            "purpose" => value
                .as_str()
                .is_some_and(install_profile_model::valid_purpose),
            "device_label" => value.as_str().is_some_and(|label| {
                label.is_empty()
                    || (!label.trim().is_empty()
                        && label.len() <= 64
                        && !label.chars().any(char::is_control))
            }),
            _ => value.as_str().is_some_and(|s| s.len() <= 256),
        };
        if !ok {
            return Err(format!("profile field {key:?} has an invalid value"));
        }
    }
    Ok(())
}

pub fn validate_profile(
    profile: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    validate_profile_with(profile, true)
}

/// Validates a profile read back from a staging record the Imager stored itself.
/// Earlier builds could store a hostname outside the strict DNS-label rule; the
/// record still loads and [`drop_invalid_hostname`] clears the bad value so the
/// installer asks again.
pub fn validate_stored_profile(
    profile: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    validate_profile_with(profile, false)
}

pub use staging_handoff_model::drop_invalid_hostname;

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
    pub fn to_padded(&self, len: u64) -> Result<Vec<u8>, String> {
        self.validate()?;
        pad_json(&serde_json::to_vec(self).map_err(|e| e.to_string())?, len)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > staging_handoff_model::MAX_BYTES {
            return Err("staged installation record is unexpectedly large".into());
        }
        let handoff: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        handoff.validate()?;
        Ok(handoff)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.staging.validate()?;
        validate_profile(&self.profile)?;
        Ok(())
    }
}

pub fn for_plan(
    operation_id: &str,
    plan: &Plan,
    esp_guid: &str,
    root_guid: &str,
    owned_boot_entry: &str,
    original_boot_order: Vec<String>,
    install_plan: Option<install_profile_model::plan::Plan>,
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
            plan: install_plan,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Handoff {
        let basis = crate::backend::plan::tests::linux_basis(
            512,
            256 << 30,
            vec![crate::backend::platform::Partition {
                number: 1,
                guid: "22222222-2222-4222-8222-222222222222".into(),
                type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                first_lba: 2048,
                last_lba: 1048575,
            }],
        );
        let plan = crate::backend::plan::compute(
            &basis,
            crate::backend::plan::Requirements {
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
            None,
            profile,
        )
    }

    #[cfg(target_os = "linux")]
    fn parse_live(bytes: &[u8]) -> Result<serde_json::Value, String> {
        crate::backend::test_support::parse_live_handoff(bytes)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_back_consumers_drop_an_invalid_hostname_the_imager_refuses() {
        let bytes = sample().to_padded(16384).unwrap();
        let shared = staging_handoff_model::Handoff::parse(&bytes).unwrap();
        let decoded = serde_json::to_value(&shared).unwrap();
        assert_eq!(parse_live(&bytes).unwrap(), decoded);

        // Imager refuses an invalid hostname when building/validating, while read-back
        // consumers (shared model and live helper) accept the handoff and drop the bad hostname.
        let mut bad_host = decoded;
        bad_host["profile"]["hostname"] = serde_json::json!("bad host");
        let bad_host_bytes = serde_json::to_vec(&bad_host).unwrap();
        assert!(Handoff::parse(&bad_host_bytes).is_err());
        let parsed_shared = staging_handoff_model::Handoff::parse(&bad_host_bytes).unwrap();
        assert!(!parsed_shared.profile.contains_key("hostname"));
        let parsed_live = parse_live(&bad_host_bytes).unwrap();
        assert!(parsed_live["profile"].get("hostname").is_none());
        assert_eq!(parsed_live, serde_json::to_value(&parsed_shared).unwrap());
    }

    tilo_test_env::scenario! {
        fn the_handoff_keeps_its_wire_format_and_maps_only_profile_keys;

        phase shared_staging_contract_remains_the_wire_format;
        {
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

        phase handoff_round_trips_validates_and_maps_only_profile_keys;
        {
            {
                let h = sample();
                let bytes = h.to_padded(4096).unwrap();
                assert_eq!(bytes.len(), 4096);
                assert!(bytes.ends_with(b"    "));
                assert_eq!(Handoff::parse(&bytes).unwrap(), h);
                assert!(h.to_padded(16).is_err());
            }

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

                let mut max_host = sample();
                max_host
                    .profile
                    .insert("hostname".into(), "h".repeat(63).into());
                assert!(validate_profile(&max_host.profile).is_ok());
                assert!(max_host.validate().is_ok());

                for bad_host in ["", &"h".repeat(64), "-tilo", "tilo-", "bad host", "tilo\n"] {
                    let mut h = sample();
                    h.profile.insert("hostname".into(), bad_host.into());
                    assert!(validate_profile(&h.profile).is_err(), "{bad_host:?}");
                    assert!(h.validate().is_err(), "{bad_host:?}");
                }
            }

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
}
