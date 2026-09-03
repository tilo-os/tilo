//! Validated host-to-live-installer profile handoff.
//!
//! The raw media block is extracted by `qos-installer-profile-extract` before
//! greetd starts. This module treats the JSON as untrusted even after its
//! corruption checksum passed: every string is bounded and allow-listed before
//! it can seed UI state or privileged-backend arguments.

use serde::Deserialize;
use std::path::Path;

pub const PROFILE_PATH: &str = "/run/qos-installer/profile.json";
pub const PROFILE_ERROR_PATH: &str = "/run/qos-installer/profile.error";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MediaProfile {
    pub schema: u8,
    pub hostname: String,
    pub username: String,
    #[serde(default)]
    pub full_name: String,
    pub locale: String,
    pub keyboard: String,
    pub timezone: String,
    pub filesystem: String,
}

impl MediaProfile {
    fn validate(self) -> Result<Self, String> {
        if self.schema != 1 {
            return Err("unsupported installation profile version".into());
        }
        if !crate::installer_app::valid_hostname(&self.hostname) {
            return Err("invalid hostname in installation profile".into());
        }
        if !crate::installer_app::valid_username(&self.username) {
            return Err("invalid username in installation profile".into());
        }
        if self.full_name.chars().count() > 100
            || self
                .full_name
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '\n' | '\r'))
        {
            return Err("invalid full name in installation profile".into());
        }
        if self.locale.is_empty()
            || self.locale.len() > 64
            || !self
                .locale
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'@' | b'-'))
        {
            return Err("invalid locale in installation profile".into());
        }
        if crate::layouts::LAYOUTS
            .iter()
            .all(|layout| layout.id != self.keyboard)
        {
            return Err("unknown keyboard layout in installation profile".into());
        }
        if self.timezone.is_empty()
            || self.timezone.len() > 128
            || self.timezone.starts_with('/')
            || self.timezone.contains("..")
            || !self
                .timezone
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-'))
        {
            return Err("invalid time zone in installation profile".into());
        }
        if !matches!(self.filesystem.as_str(), "btrfs" | "ext4" | "xfs") {
            return Err("invalid filesystem in installation profile".into());
        }
        Ok(self)
    }

    pub fn keyboard_display_name(&self) -> &'static str {
        crate::layouts::LAYOUTS
            .iter()
            .find(|layout| layout.id == self.keyboard)
            .map_or(crate::layouts::DEFAULT_DISPLAY_NAME, |layout| {
                layout.display_name
            })
    }
}

/// Load and validate a profile extracted from the live medium.
///
/// # Errors
///
/// Returns an error when the profile cannot be read, exceeds the size limit,
/// contains malformed JSON, or fails validation.
pub fn load_from(path: &Path) -> Result<Option<MediaProfile>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read installation profile: {error}")),
    };
    if bytes.len() > 32 * 1024 {
        return Err("installation profile is unexpectedly large".into());
    }
    serde_json::from_slice::<MediaProfile>(&bytes)
        .map_err(|error| format!("installation profile is malformed: {error}"))?
        .validate()
        .map(Some)
}

/// Load and validate the profile extracted at [`PROFILE_PATH`].
///
/// # Errors
///
/// Returns the same read, size, parse, or validation errors as [`load_from`].
pub fn load() -> Result<Option<MediaProfile>, String> {
    load_from(Path::new(PROFILE_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_imager_contract_and_rejects_destructive_fields() {
        let good = br#"{"schema":1,"hostname":"studio","username":"ada","full_name":"ada","locale":"en_US.UTF-8","keyboard":"de","timezone":"Europe/Berlin","filesystem":"btrfs"}"#;
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), good).unwrap();
        let profile = load_from(file.path()).unwrap().unwrap();
        assert_eq!(profile.keyboard_display_name(), "German (QWERTZ)");

        let bad = br#"{"schema":1,"hostname":"studio","username":"ada","full_name":"ada","locale":"en_US.UTF-8","keyboard":"de","timezone":"Europe/Berlin","filesystem":"btrfs","target":"/dev/sda"}"#;
        std::fs::write(file.path(), bad).unwrap();
        assert!(load_from(file.path()).is_err());
    }

    #[test]
    fn rejects_command_and_path_injection() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let bad = br#"{"schema":1,"hostname":"bad host","username":"root","full_name":"x:wheel","locale":"../../x","keyboard":"nope","timezone":"../../etc","filesystem":"zfs"}"#;
        std::fs::write(file.path(), bad).unwrap();
        assert!(load_from(file.path()).is_err());
    }

    #[test]
    fn rejects_each_invalid_field_independently() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let good = serde_json::json!({
            "schema": 1,
            "hostname": "studio",
            "username": "ada",
            "full_name": "ada",
            "locale": "en_US.UTF-8",
            "keyboard": "de",
            "timezone": "Europe/Berlin",
            "filesystem": "btrfs",
        });
        let cases = [
            ("schema", "version", serde_json::json!(2)),
            ("hostname", "hostname", serde_json::json!("bad host")),
            ("username", "username", serde_json::json!("root")),
            ("full_name", "full name", serde_json::json!("Bad:Name")),
            ("locale", "locale", serde_json::json!("../../etc")),
            ("keyboard", "keyboard", serde_json::json!("not-a-layout")),
            ("timezone", "time zone", serde_json::json!("../../etc")),
            ("filesystem", "filesystem", serde_json::json!("zfs")),
        ];
        for (field, error_fragment, value) in cases {
            let mut candidate = good.clone();
            candidate[field] = value;
            std::fs::write(file.path(), serde_json::to_vec(&candidate).unwrap()).unwrap();
            let error = load_from(file.path())
                .expect_err("one invalid consumer field must reject the entire profile");
            assert!(
                error.contains(error_fragment),
                "{field} produced an unhelpful error: {error}"
            );
        }
    }
}
