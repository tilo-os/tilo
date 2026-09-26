use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationKind {
    Extension,
    MimeType,
    UriScheme,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Association {
    pub kind: AssociationKind,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublishedApplication {
    /// Stable opaque identifier minted by the guest bridge. It is not a path
    /// and cannot be used to ask Windows to run arbitrary input.
    pub id: String,
    #[serde(rename = "displayName", alias = "display_name")]
    pub display_name: String,
    /// Allow-listed `RemoteApp` alias registered in the guest, never an executable path.
    #[serde(rename = "remoteAppAlias", alias = "remote_app_alias")]
    pub remote_app_alias: String,
    /// Content hash of the icon returned separately by the guest bridge.
    #[serde(rename = "iconSha256", alias = "icon_sha256")]
    pub icon_sha256: Option<String>,
    pub associations: Vec<Association>,
    /// The guest has a safely typed, administrator-owned uninstall record.
    /// No command or product code is exposed through the catalogue.
    #[serde(default, rename = "uninstallAvailable", alias = "uninstall_available")]
    pub uninstall_available: bool,
}

impl PublishedApplication {
    pub fn validate(&self) -> Result<(), CatalogError> {
        validate_identifier("application id", &self.id)?;
        validate_identifier("RemoteApp alias", &self.remote_app_alias)?;
        if self.display_name.trim().is_empty() || self.display_name.chars().any(char::is_control) {
            return Err(CatalogError::InvalidDisplayName);
        }
        if self
            .icon_sha256
            .as_ref()
            .is_some_and(|hash| !is_sha256(hash))
        {
            return Err(CatalogError::InvalidIconHash);
        }
        for association in &self.associations {
            validate_association(association)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub generation: u64,
    pub applications: Vec<PublishedApplication>,
}

impl Catalog {
    pub fn validate(&self) -> Result<(), CatalogError> {
        let mut ids = HashSet::new();
        let mut aliases = HashSet::new();
        for application in &self.applications {
            application.validate()?;
            if !ids.insert(application.id.as_str()) {
                return Err(CatalogError::DuplicateId(application.id.clone()));
            }
            if !aliases.insert(application.remote_app_alias.as_str()) {
                return Err(CatalogError::DuplicateAlias(
                    application.remote_app_alias.clone(),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn application(&self, id: &str) -> Option<&PublishedApplication> {
        self.applications
            .iter()
            .find(|application| application.id == id)
    }
}

fn validate_identifier(label: &'static str, value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CatalogError::InvalidIdentifier(label));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_association(association: &Association) -> Result<(), CatalogError> {
    if association.value.is_empty()
        || association.value.len() > 255
        || association.value.chars().any(char::is_control)
    {
        return Err(CatalogError::InvalidAssociation);
    }
    match association.kind {
        AssociationKind::Extension => {
            if !association.value.starts_with('.')
                || !association
                    .value
                    .bytes()
                    .skip(1)
                    .all(|byte| byte.is_ascii_alphanumeric())
            {
                return Err(CatalogError::InvalidAssociation);
            }
        }
        AssociationKind::MimeType => {
            if !association.value.contains('/') || association.value.contains(' ') {
                return Err(CatalogError::InvalidAssociation);
            }
        }
        AssociationKind::UriScheme => {
            let mut bytes = association.value.bytes();
            if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
                || !bytes
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
            {
                return Err(CatalogError::InvalidAssociation);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CatalogError {
    #[error("invalid {0}")]
    InvalidIdentifier(&'static str),
    #[error("application display name is empty or contains control characters")]
    InvalidDisplayName,
    #[error("icon hash is not SHA-256")]
    InvalidIconHash,
    #[error("invalid file or URI association")]
    InvalidAssociation,
    #[error("duplicate application id {0:?}")]
    DuplicateId(String),
    #[error("duplicate RemoteApp alias {0:?}")]
    DuplicateAlias(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> PublishedApplication {
        PublishedApplication {
            id: id.to_string(),
            display_name: "Example".to_string(),
            remote_app_alias: format!("tilo-{id}"),
            icon_sha256: None,
            associations: vec![Association {
                kind: AssociationKind::Extension,
                value: ".abc".to_string(),
            }],
            uninstall_available: false,
        }
    }

    #[test]
    fn publication_catalogues_reject_paths_duplicates_and_unsafe_uninstallers() {
        // Phase `rejects_executable_paths_as_aliases`.
        {
            let mut application = app("example");
            application.remote_app_alias = "C:\\Windows\\notepad.exe".to_string();
            assert_eq!(
                application.validate(),
                Err(CatalogError::InvalidIdentifier("RemoteApp alias"))
            );
        }
        // Phase `older_catalogues_default_to_no_safe_uninstaller`.
        {
            let application: PublishedApplication = serde_json::from_str(
                r#"{"id":"example","displayName":"Example","remoteAppAlias":"tilo-example","iconSha256":null,"associations":[]}"#,
            )
            .unwrap();
            assert!(!application.uninstall_available);
        }
        // Phase `duplicate_ids_are_rejected`.
        {
            let catalog = Catalog {
                generation: 1,
                applications: vec![app("same"), app("same")],
            };
            assert!(matches!(
                catalog.validate(),
                Err(CatalogError::DuplicateId(_))
            ));
        }
    }
}
