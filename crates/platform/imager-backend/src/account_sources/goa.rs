//! GOA metadata definitions shared by review and selected capture.
use super::{Candidate, Inventory, Origin, Provider, Service};
use account_custody::OAuthCloud;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
use tilo_secrets::owner_migration::GoaProvider;
use zeroize::Zeroize;

struct PrivateFields(BTreeMap<String, String>);
impl std::ops::Deref for PrivateFields {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for PrivateFields {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}
pub struct Definition {
    pub id: String,
    pub provider: GoaProvider,
    pub identity: String,
    pub label: String,
    pub fields: BTreeMap<String, String>,
}
impl Definition {
    pub fn cloud(&self) -> OAuthCloud {
        match self.provider {
            GoaProvider::Google => OAuthCloud::GoogleDrive,
            GoaProvider::Microsoft => OAuthCloud::OneDrive,
        }
    }
    pub fn source_id(&self) -> String {
        let mut hash = Sha256::new();
        // Secrets never enter a public fingerprint; registration overrides and
        // selected identity do, so edits cannot substitute another connection.
        for value in [
            self.id.as_str(),
            self.provider.as_str(),
            self.identity.as_str(),
            self.label.as_str(),
            self.fields.get("OAuth2ClientId").map_or("", String::as_str),
            self.fields.get("OAuth2TokenUri").map_or("", String::as_str),
            self.fields
                .get("OAuth2AuthorizationUri")
                .map_or("", String::as_str),
            self.fields.get("FilesEnabled").map_or("", String::as_str),
            self.fields.get("MailEnabled").map_or("", String::as_str),
            self.fields
                .get("CalendarEnabled")
                .map_or("", String::as_str),
            self.fields
                .get("ContactsEnabled")
                .map_or("", String::as_str),
        ] {
            hash.update(value.len().to_le_bytes());
            hash.update(value.as_bytes());
        }
        format!("goa-source-{}", hex::encode(hash.finalize()))
    }
    pub fn files_enabled(&self) -> bool {
        self.fields
            .get("FilesEnabled")
            .is_some_and(|value| value == "true")
    }

    pub fn services(&self) -> Vec<Service> {
        let mut services = Vec::new();
        if self.files_enabled() {
            services.push(Service::Files);
        }
        if self
            .fields
            .get("MailEnabled")
            .is_some_and(|value| value == "true")
        {
            services.push(Service::Mail);
        }
        if self
            .fields
            .get("CalendarEnabled")
            .is_some_and(|value| value == "true")
        {
            services.push(Service::Calendar);
        }
        if self
            .fields
            .get("ContactsEnabled")
            .is_some_and(|value| value == "true")
        {
            services.push(Service::Contacts);
        }
        services
    }

    pub fn supported(&self) -> bool {
        let endpoint = match self.provider {
            GoaProvider::Google => "https://oauth2.googleapis.com/token",
            GoaProvider::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        };
        let authorization = match self.provider {
            GoaProvider::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            GoaProvider::Microsoft => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
            }
        };
        self.fields
            .get("OAuth2TokenUri")
            .is_none_or(|value| value == endpoint)
            && self
                .fields
                .get("OAuth2AuthorizationUri")
                .is_none_or(|value| value == authorization)
            && self.files_enabled()
    }
    pub fn candidate(&self, path: &Path) -> Candidate {
        Candidate {
            id: self.source_id(),
            provider: match self.provider {
                GoaProvider::Google => Provider::Google,
                GoaProvider::Microsoft => Provider::Microsoft,
            },
            origin: Origin::Gnome,
            label: self.label.clone(),
            identity: Some(self.label.clone()),
            server: None,
            profile: path.to_owned(),
            needs_sign_in: true,
            can_import_credentials: self.supported(),
            services: self.services(),
        }
    }
}
impl Drop for Definition {
    fn drop(&mut self) {
        for value in self.fields.values_mut() {
            value.zeroize();
        }
    }
}
pub fn definitions(text: &str, private: bool) -> Vec<Definition> {
    let mut allowed = vec![
        "Provider",
        "Identity",
        "PresentationIdentity",
        "OAuth2ClientId",
        "OAuth2TokenUri",
        "OAuth2AuthorizationUri",
        "FilesEnabled",
        "MailEnabled",
        "CalendarEnabled",
        "ContactsEnabled",
    ];
    if private {
        allowed.push("OAuth2ClientSecret");
    }
    super::sections(text, &allowed)
        .into_iter()
        .filter_map(|(section, fields)| {
            let mut fields = PrivateFields(fields);
            let id = section.strip_prefix("Account ")?;
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return None;
            }
            let provider = match fields.get("Provider")?.as_str() {
                "google" => GoaProvider::Google,
                "ms_graph" => GoaProvider::Microsoft,
                _ => return None,
            };
            let identity = fields.get("Identity")?.clone();
            if identity.is_empty() || identity.len() > 256 || identity.chars().any(char::is_control)
            {
                return None;
            }
            let label = fields
                .get("PresentationIdentity")
                .unwrap_or(&identity)
                .clone();
            if label.is_empty() || label.len() > 256 || label.chars().any(char::is_control) {
                return None;
            }
            Some(Definition {
                id: id.into(),
                provider,
                identity,
                label,
                fields: std::mem::take(&mut fields.0),
            })
        })
        .collect()
}
pub(super) fn discover(text: &str, path: &Path, inventory: &mut Inventory) {
    for definition in definitions(text, false) {
        let candidate = definition.candidate(path);
        inventory.candidates.retain(|old| {
            !(old.origin == Origin::Gnome
                && !old.id.starts_with("goa-source-")
                && old.provider == candidate.provider
                && old.identity == candidate.identity)
        });
        inventory.candidates.push(candidate);
    }
}
