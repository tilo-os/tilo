//! Public installation preferences authenticated inside the prepared package.
//! Passwords and destructive choices never belong in this record.
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicInstallProfile {
    pub schema: u32,
    #[serde(default = "install_profile_model::default_purpose")]
    pub purpose: String,
    pub username: String,
    pub full_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_label: String,
    pub hostname: String,
    pub locale: String,
    pub keyboard: String,
    pub timezone: String,
    pub filesystem: String,
}

impl PublicInstallProfile {
    pub fn from_draft(draft: &crate::imager_model::InstallProfile) -> io::Result<Self> {
        draft.validate_media_profile().map_err(io::Error::other)?;
        let bytes = draft.to_media_profile_json().map_err(io::Error::other)?;
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        // The ciphertext expectation cannot refer to its own enclosing package.
        value
            .as_object_mut()
            .ok_or_else(super::invalid)?
            .remove("prepared_setup");
        serde_json::from_value(value).map_err(io::Error::other)
    }

    pub fn to_draft(&self) -> io::Result<crate::imager_model::InstallProfile> {
        self.validate()?;
        let mut draft = crate::imager_model::InstallProfile {
            hostname: self.hostname.clone(),
            device_label: self.device_label.clone(),
            locale: self.locale.clone(),
            keyboard: self.keyboard.clone(),
            timezone: self.timezone.clone(),
            purpose: self.purpose.clone(),
            filesystem: crate::imager_model::Filesystem::from_key(&self.filesystem)
                .ok_or_else(super::invalid)?,
            ..Default::default()
        };
        draft.user.username.clone_from(&self.username);
        draft.user.full_name.clone_from(&self.full_name);
        Ok(draft)
    }

    /// Validate the canonical secret-free wire record without a frontend draft.
    pub fn validate(&self) -> io::Result<()> {
        use install_profile_model as rules;
        let canonical = [
            &self.hostname,
            &self.username,
            &self.full_name,
            &self.locale,
            &self.keyboard,
            &self.timezone,
        ]
        .into_iter()
        .all(|value| value.trim() == value.as_str());
        if self.schema != 1
            || !canonical
            || !rules::valid_purpose(&self.purpose)
            || !rules::valid_device_label(&self.device_label)
            || !rules::valid_hostname(&self.hostname)
            || !rules::valid_username(&self.username)
            || !rules::valid_full_name(&self.full_name)
            || !rules::valid_locale(&self.locale)
            || !rules::KEYBOARDS.contains(&self.keyboard.as_str())
            || !rules::valid_timezone_name(&self.timezone)
            || !rules::FILESYSTEMS.contains(&self.filesystem.as_str())
        {
            return Err(super::invalid());
        }
        Ok(())
    }
}
