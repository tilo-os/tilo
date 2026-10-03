//! The staging ESP handoff read by the live installer (`tilo-install
//! --internal-handoff`). A hint, never authority: the installer revalidates
//! every identity and extent and the person still confirms.
//!
//! On the ESP the file is `/EFI/tilo-staging/handoff.json`, a fixed-size
//! placeholder the exporter reserves (see [`super::fat`]). Its content is the
//! compact JSON below followed by ASCII spaces up to the file's size; any
//! JSON parser that permits trailing whitespace (e.g. `serde_json::from_slice`,
//! Python `json.loads`) reads it unchanged.

use serde::{Deserialize, Serialize};

use super::plan::{Extent, Plan};

pub const HANDOFF_FILE: &str = "handoff.json";
pub const SCHEMA: u32 = 1;

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

impl Handoff {
    /// Build the handoff for a plan whose partitions now carry `esp_guid` and
    /// `root_guid`. `original_boot_order` comes from the helper's live
    /// firmware read (the Windows probe basis has none).
    pub fn for_plan(
        operation_id: &str,
        plan: &Plan,
        esp_guid: &str,
        root_guid: &str,
        owned_boot_entry: &str,
        original_boot_order: Vec<String>,
        profile: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            schema: SCHEMA,
            operation_id: operation_id.into(),
            host_os: plan.basis.host_os().into(),
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

    /// Compact JSON padded with spaces to exactly `len` bytes.
    pub fn to_padded(&self, len: u64) -> Result<Vec<u8>, String> {
        pad_json(&serde_json::to_vec(self).map_err(|e| e.to_string())?, len)
    }

    /// Parse placeholder bytes (JSON plus trailing whitespace).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let handoff: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        handoff.validate()?;
        Ok(handoff)
    }

    /// Internal consistency the installer can check before revalidating
    /// against the live disk.
    pub fn validate(&self) -> Result<(), String> {
        let extent_ok = |first: u64, last: u64| first > 0 && first <= last;
        if self.schema != SCHEMA
            || !matches!(self.host_os.as_str(), "windows" | "linux")
            || !super::is_guid(&self.disk_guid)
            || !super::is_guid(&self.staging_esp.partuuid)
            || !super::is_guid(&self.staging_root.partuuid)
            || self.staging_esp.partuuid == self.staging_root.partuuid
            || !extent_ok(self.staging_esp.first_lba, self.staging_esp.last_lba)
            || !extent_ok(self.staging_root.first_lba, self.staging_root.last_lba)
            || !extent_ok(self.target_extent.first_lba, self.target_extent.last_lba)
            || self.target_extent.last_lba + 1 != self.staging_esp.first_lba
            || self.staging_esp.last_lba + 1 != self.staging_root.first_lba
            || self.owned_boot_entry.len() != 4
            || self.owned_boot_entry != self.owned_boot_entry.to_ascii_uppercase()
            || super::efi::parse_boot_number(&self.owned_boot_entry).is_err()
            || self.original_boot_order.contains(&self.owned_boot_entry)
        {
            return Err("handoff is internally inconsistent".into());
        }
        validate_profile(&self.profile)
    }
}

/// `json` followed by ASCII spaces to exactly `len` bytes.
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
        Handoff::for_plan(
            "op-1",
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
            assert!(h.validate().is_err());
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
