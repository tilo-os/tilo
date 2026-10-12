//! Public installation preferences authenticated inside the prepared package.
//! Passwords and destructive choices never belong in this record.
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicInstallProfile {
    pub schema: u32,
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
    pub fn from_draft(draft: &crate::model::InstallProfile) -> io::Result<Self> {
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

    pub fn validate(&self) -> io::Result<()> {
        let filesystem =
            crate::model::Filesystem::from_key(&self.filesystem).ok_or_else(super::invalid)?;
        let mut draft = crate::model::InstallProfile {
            hostname: self.hostname.clone(),
            device_label: self.device_label.clone(),
            locale: self.locale.clone(),
            keyboard: self.keyboard.clone(),
            timezone: self.timezone.clone(),
            filesystem,
            ..Default::default()
        };
        draft.user.username.clone_from(&self.username);
        draft.user.full_name.clone_from(&self.full_name);
        draft.validate_media_profile().map_err(io::Error::other)?;
        if self.schema != 1 || Self::from_draft(&draft)? != *self {
            return Err(super::invalid());
        }
        Ok(())
    }

    pub fn to_draft(&self) -> io::Result<crate::model::InstallProfile> {
        self.validate()?;
        let mut draft = crate::model::InstallProfile {
            hostname: self.hostname.clone(),
            device_label: self.device_label.clone(),
            locale: self.locale.clone(),
            keyboard: self.keyboard.clone(),
            timezone: self.timezone.clone(),
            filesystem: crate::model::Filesystem::from_key(&self.filesystem)
                .ok_or_else(super::invalid)?,
            ..Default::default()
        };
        draft.user.username.clone_from(&self.username);
        draft.user.full_name.clone_from(&self.full_name);
        Ok(draft)
    }
}
