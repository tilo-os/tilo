//! Validated host-to-live-installer profile handoff.
//!
//! The raw media block is extracted by `tilo-installer-profile-extract` before
//! greetd starts. This module treats the JSON as untrusted even after its
//! corruption checksum passed. Each non-secret field is validated separately:
//! sound values seed the installer, while missing or invalid values remain
//! unset so the graphical flow asks for only those repairs.

use serde::Deserialize;
use std::path::Path;

pub const PROFILE_PATH: &str = "/run/tilo-installer/profile.json";
pub const PROFILE_ERROR_PATH: &str = "/run/tilo-installer/profile.error";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaProfileField {
    Hostname,
    Username,
    FullName,
    Locale,
    Keyboard,
    Timezone,
    Filesystem,
}

impl MediaProfileField {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hostname => "hostname",
            Self::Username => "username",
            Self::FullName => "full name",
            Self::Locale => "language and locale",
            Self::Keyboard => "keyboard layout",
            Self::Timezone => "time zone",
            Self::Filesystem => "filesystem",
        }
    }
}

/// Independently validated, secret-free values recovered from the medium.
/// `None` means that field must be completed on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaProfile {
    pub purpose: String,
    pub device_label: String,
    pub prepared_setup: Option<install_profile_model::PreparedSetupExpectation>,
    /// Public, TPM-sealed sidecar carried from the Imager's same-computer
    /// preparation. Its only secret is a TPM private blob.
    pub tpm_handoff: Option<String>,
    pub windows_preparation: Option<account_custody::hints::media::Expectation>,
    pub hostname: Option<String>,
    pub username: Option<String>,
    pub full_name: Option<String>,
    pub locale: Option<String>,
    pub keyboard: Option<String>,
    pub timezone: Option<String>,
    pub filesystem: Option<String>,
    pub repairs: Vec<MediaProfileField>,
}

impl MediaProfile {
    pub fn keyboard_display_name(&self) -> Option<&'static str> {
        let keyboard = self.keyboard.as_deref()?;
        crate::layouts::LAYOUTS
            .iter()
            .find(|layout| layout.id == keyboard)
            .map(|layout| layout.display_name)
    }

    pub fn repair_summary(&self) -> String {
        let fields = self
            .repairs
            .iter()
            .map(|field| field.label())
            .collect::<Vec<_>>();
        match fields.as_slice() {
            [] => "Your setup choices are ready. Choose where to install Tilo.".to_string(),
            [only] => format!("Your other setup choices are ready. Please check the {only}."),
            _ => format!(
                "Your other setup choices are ready. Please check: {}.",
                fields.join(", ")
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMediaProfile {
    schema: u8,
    #[serde(default = "install_profile_model::default_purpose")]
    purpose: String,
    #[serde(default)]
    device_label: String,
    #[serde(default, deserialize_with = "present_expectation")]
    prepared_setup: Option<install_profile_model::PreparedSetupExpectation>,
    #[serde(default)]
    tpm_handoff: Option<String>,
    #[serde(default)]
    windows_preparation: Option<account_custody::hints::media::Expectation>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    locale: Option<String>,
    #[serde(default)]
    keyboard: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    filesystem: Option<String>,
}

fn present_expectation<'de, D>(
    deserializer: D,
) -> Result<Option<install_profile_model::PreparedSetupExpectation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    install_profile_model::PreparedSetupExpectation::deserialize(deserializer).map(Some)
}

fn retain_valid<F>(
    value: Option<String>,
    field: MediaProfileField,
    repairs: &mut Vec<MediaProfileField>,
    valid: F,
) -> Option<String>
where
    F: FnOnce(&str) -> bool,
{
    match value {
        Some(value) if valid(&value) => Some(value),
        _ => {
            repairs.push(field);
            None
        }
    }
}

impl RawMediaProfile {
    fn validate(self, zoneinfo_root: &Path) -> Result<MediaProfile, String> {
        if self.prepared_setup.is_some() && self.windows_preparation.is_some() {
            return Err("Windows suggestions cannot claim a prepared Linux identity".into());
        }
        if self
            .tpm_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.len() > 64 * 1024)
        {
            return Err("The prepared TPM handoff exceeds its bound".into());
        }
        if self.tpm_handoff.is_some() && self.prepared_setup.is_none() {
            return Err("A prepared TPM handoff needs its prepared package".into());
        }
        if self.schema != 1 {
            return Err("unsupported installation profile version".into());
        }
        if !install_profile_model::valid_purpose(&self.purpose) {
            return Err("unsupported installation purpose".into());
        }
        if !install_profile_model::valid_device_label(&self.device_label) {
            return Err("invalid installation device label".into());
        }
        let mut repairs = Vec::new();
        let hostname = retain_valid(
            self.hostname,
            MediaProfileField::Hostname,
            &mut repairs,
            install_profile_model::valid_hostname,
        );
        let username = retain_valid(
            self.username,
            MediaProfileField::Username,
            &mut repairs,
            install_profile_model::valid_username,
        );
        // Full name is optional in both the imager and installer. Omitting it
        // is therefore a valid configured choice; only a supplied bad value
        // needs an on-device repair.
        let full_name = match self.full_name {
            None => Some(String::new()),
            value => retain_valid(
                value,
                MediaProfileField::FullName,
                &mut repairs,
                install_profile_model::valid_full_name,
            ),
        };
        let locale = retain_valid(
            self.locale,
            MediaProfileField::Locale,
            &mut repairs,
            install_profile_model::valid_locale,
        );
        let keyboard = retain_valid(
            self.keyboard,
            MediaProfileField::Keyboard,
            &mut repairs,
            |keyboard| {
                crate::layouts::LAYOUTS
                    .iter()
                    .any(|layout| layout.id == keyboard)
            },
        );
        let timezone = retain_valid(
            self.timezone,
            MediaProfileField::Timezone,
            &mut repairs,
            |timezone| {
                install_profile_model::valid_timezone_name(timezone)
                    && zoneinfo_root.join(timezone).is_file()
            },
        );
        let filesystem = retain_valid(
            self.filesystem,
            MediaProfileField::Filesystem,
            &mut repairs,
            |filesystem| install_profile_model::FILESYSTEMS.contains(&filesystem),
        );
        Ok(MediaProfile {
            purpose: self.purpose,
            device_label: self.device_label,
            prepared_setup: self.prepared_setup,
            tpm_handoff: self.tpm_handoff,
            windows_preparation: self.windows_preparation,
            hostname,
            username,
            full_name,
            locale,
            keyboard,
            timezone,
            filesystem,
            repairs,
        })
    }
}

/// Load and independently validate a profile extracted from the live medium.
///
/// # Errors
///
/// Returns an error only when the envelope cannot be trusted as a profile at
/// all: unreadable/oversized input, malformed JSON, an unsupported schema, or
/// unexpected fields. Missing and semantically invalid setup values instead
/// become explicit on-device repairs.
pub fn load_from(path: &Path) -> Result<Option<MediaProfile>, String> {
    load_from_with_zoneinfo(path, Path::new("/usr/share/zoneinfo"))
}

fn load_from_with_zoneinfo(
    path: &Path,
    zoneinfo_root: &Path,
) -> Result<Option<MediaProfile>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read installation profile: {error}")),
    };
    if bytes.len() > 32 * 1024 {
        return Err("installation profile is unexpectedly large".into());
    }
    serde_json::from_slice::<RawMediaProfile>(&bytes)
        .map_err(|_| "installation profile is malformed".to_string())?
        .validate(zoneinfo_root)
        .map(Some)
}

/// Validate the secret-free profile object carried by a staged (USB-free)
/// installation handoff. It is the same object Tilo Imager writes to media;
/// `schema` may be omitted there because the handoff has its own schema.
///
/// # Errors
///
/// Returns an error for an unsupported schema or fields outside the media
/// profile. Individual invalid values become repairs, exactly as for media.
pub fn validate_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<MediaProfile, String> {
    validate_object_with_zoneinfo(object, Path::new("/usr/share/zoneinfo"))
}

pub(crate) fn validate_object_with_zoneinfo(
    object: &serde_json::Map<String, serde_json::Value>,
    zoneinfo_root: &Path,
) -> Result<MediaProfile, String> {
    let mut object = object.clone();
    object
        .entry("schema")
        .or_insert_with(|| serde_json::Value::from(1));
    serde_json::from_value::<RawMediaProfile>(serde_json::Value::Object(object))
        .map_err(|_| "installation profile is malformed".to_string())?
        .validate(zoneinfo_root)
}

/// Load and validate the profile extracted at [`PROFILE_PATH`].
///
/// # Errors
///
/// Returns an error when the profile cannot be read or its envelope is
/// malformed. Individual missing or invalid setup fields are retained as
/// repair requests in the returned profile rather than failing the envelope.
pub fn load() -> Result<Option<MediaProfile>, String> {
    load_from(Path::new(PROFILE_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> serde_json::Value {
        serde_json::json!({
            "schema": 1,
            "hostname": "studio",
            "username": "ada",
            "full_name": "ada",
            "locale": "en_US.UTF-8",
            "keyboard": "de",
            "timezone": "Europe/Berlin",
            "filesystem": "btrfs",
        })
    }

    #[test]
    fn purpose_is_validated_and_legacy_profiles_keep_computer_defaults() {
        let parse = |purpose: Option<&str>| {
            let mut object = serde_json::Map::new();
            if let Some(purpose) = purpose {
                object.insert("purpose".into(), purpose.into());
            }
            validate_object(&object)
        };
        assert_eq!(parse(None).unwrap().purpose, "computer");
        assert_eq!(parse(Some("server")).unwrap().purpose, "server");
        assert!(parse(Some("other-os")).is_err());
    }

    #[test]
    fn media_profile_accepts_the_contract_and_repairs_only_bad_values() {
        // Phase `accepts_the_imager_contract_and_rejects_destructive_fields`.
        {
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), serde_json::to_vec(&good()).unwrap()).unwrap();
            let profile = load_from(file.path()).unwrap().unwrap();
            assert_eq!(profile.keyboard_display_name(), Some("German (QWERTZ)"));
            assert!(profile.repairs.is_empty());

            let mut bad = good();
            bad["target"] = serde_json::json!("/dev/sda");
            std::fs::write(file.path(), serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(load_from(file.path()).is_err());
        }
        // Phase `retains_each_sound_field_while_requesting_one_repair`.
        {
            let zones = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(zones.path().join("Europe")).unwrap();
            std::fs::write(zones.path().join("Europe/Berlin"), b"zone").unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            let cases = [
                (
                    "hostname",
                    MediaProfileField::Hostname,
                    serde_json::json!("bad host"),
                ),
                (
                    "username",
                    MediaProfileField::Username,
                    serde_json::json!("root"),
                ),
                (
                    "full_name",
                    MediaProfileField::FullName,
                    serde_json::json!("Bad:Name"),
                ),
                (
                    "locale",
                    MediaProfileField::Locale,
                    // Syntactically plausible is not enough: the installed image
                    // must actually carry the selected locale data.
                    serde_json::json!("zz_ZZ.UTF-8"),
                ),
                (
                    "keyboard",
                    MediaProfileField::Keyboard,
                    serde_json::json!("not-a-layout"),
                ),
                (
                    "timezone",
                    MediaProfileField::Timezone,
                    serde_json::json!("Europe/Missing"),
                ),
                (
                    "filesystem",
                    MediaProfileField::Filesystem,
                    serde_json::json!("zfs"),
                ),
            ];
            for (field, expected, value) in cases {
                let mut candidate = good();
                candidate[field] = value;
                std::fs::write(file.path(), serde_json::to_vec(&candidate).unwrap()).unwrap();
                let profile = load_from_with_zoneinfo(file.path(), zones.path())
                    .unwrap()
                    .unwrap();
                assert_eq!(profile.repairs, vec![expected], "wrong repair for {field}");
                assert_eq!(
                    profile.username.as_deref(),
                    (field != "username").then_some("ada")
                );
                assert_eq!(
                    profile.hostname.as_deref(),
                    (field != "hostname").then_some("studio")
                );
            }
        }
        // Phase `missing_values_are_repairs_but_schema_and_shape_still_fail_closed`.
        {
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut missing = good();
            missing.as_object_mut().unwrap().remove("hostname");
            std::fs::write(file.path(), serde_json::to_vec(&missing).unwrap()).unwrap();
            let profile = load_from(file.path()).unwrap().unwrap();
            assert!(profile.hostname.is_none());
            assert!(profile.repairs.contains(&MediaProfileField::Hostname));

            let mut optional_name = good();
            optional_name.as_object_mut().unwrap().remove("full_name");
            std::fs::write(file.path(), serde_json::to_vec(&optional_name).unwrap()).unwrap();
            let profile = load_from(file.path()).unwrap().unwrap();
            assert_eq!(profile.full_name.as_deref(), Some(""));
            assert!(profile.repairs.is_empty());

            let mut schema = good();
            schema["schema"] = serde_json::json!(2);
            std::fs::write(file.path(), serde_json::to_vec(&schema).unwrap()).unwrap();
            assert!(load_from(file.path()).is_err());

            std::fs::write(file.path(), b"not json").unwrap();
            assert!(load_from(file.path()).is_err());
        }
    }
}
