//! GOA metadata definitions shared by review and selected capture.
use super::{Candidate, Inventory, Origin, Provider, Service};
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
            self.fields.get("MailEnabled").map_or("", String::as_str),
            self.fields
                .get("CalendarEnabled")
                .map_or("", String::as_str),
        ] {
            hash.update(value.len().to_le_bytes());
            hash.update(value.as_bytes());
        }
        format!("goa-source-{}", hex::encode(hash.finalize()))
    }
    pub fn services(&self) -> Vec<Service> {
        let mut services = Vec::new();
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
        services
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
            can_import_credentials: false,
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
        "MailEnabled",
        "CalendarEnabled",
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
