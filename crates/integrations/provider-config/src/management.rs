//! Host-authorized provider administration, separate from inference authority.
//! No provider key passes here: keys are kept where the person's workspace
//! is, and set there (`tilo.models`). The descriptions are settings of the
//! person and the device: [`snapshot`] reads them from this process's
//! settings and [`replace`] sets them there, as the settings app's own
//! setters do.

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    ModelDescriptor, ProviderConfig,
    protocol::{ErrorKind, ModelError},
};

/// Authoritative backend settings and their compare-and-swap revision.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub config: ProviderConfig,
    pub revision: String,
}

/// Administration returns data rather than borrowing UI callbacks. Browser
/// transports preserve this contract using the platform's Send-safe fetch API.
pub type AdminFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModelError>> + Send + 'a>>;

/// An explicitly granted capability, not part of an ordinary model session.
/// Hosts bind the authenticated user before providing this interface. Every
/// operation remains subject to backend authorization and durable-write checks.
/// Dropping a mutation abandons its response, not its durable commit. Refresh
/// configuration/status before deciding whether an interrupted write needs retry.
pub trait ProviderAdmin: Send + Sync {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot>;
    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot>;
    /// The models the provider lists, asked as its sign-in or, for a local
    /// server, as itself: also the proof that the endpoint answers.
    /// [`crate::protocol::ErrorKind::Unsupported`] when it lists none.
    fn list_models(&self, profile_id: String) -> AdminFuture<'_, Vec<ModelDescriptor>>;
}

/// The descriptions this process's settings hold, with their revision: what an edit is checked
/// against.
///
/// # Errors
/// The stored descriptions are out of bounds.
pub fn snapshot() -> Result<ProviderSnapshot, ModelError> {
    let config = ProviderConfig::load();
    validate(&config)?;
    revision(config)
}

/// Set the descriptions to `config` if they are still those at `expected_revision`.
///
/// # Errors
/// [`ErrorKind::Conflict`] when they changed since, [`ErrorKind::PermissionDenied`] when the edit
/// would mint or retarget a model the provisioner installed, [`ErrorKind::InvalidRequest`] when
/// `config` is out of bounds, [`ErrorKind::Unavailable`] when the settings refused the write.
pub fn replace(
    expected_revision: &str,
    config: ProviderConfig,
) -> Result<ProviderSnapshot, ModelError> {
    let previous = ProviderConfig::load();
    if revision(previous.clone())?.revision != expected_revision {
        return Err(ModelError::new(ErrorKind::Conflict));
    }
    admitted(&previous, &config)?;
    config
        .save()
        .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    revision(config)
}

/// The setters an app that administers the model providers besides Settings (Model Providers)
/// may invoke: those of the person's hosted providers and of each device's own, as grant
/// patterns over their addresses. Each write of theirs is held to [`check_write`].
pub const SETTERS: &[&str] = &[
    "settings/agent-providers-person#set-*",
    "devices/*/settings/agent-providers#set-*",
];

/// Whether writing `values` to `section` of the settings `document` keeps the providers within
/// bounds and the provisioner's metadata its own; any other section passes.
///
/// # Errors
/// What the write would break.
pub fn check_write(
    document: &Map<String, Value>,
    section: &str,
    values: &Map<String, Value>,
) -> Result<(), String> {
    if section != crate::SETTINGS_SECTION && section != crate::HOSTED_SECTION {
        return Ok(());
    }
    let mut written = document.clone();
    settings_store::merge_section(&mut written, section, values.clone());
    admitted(
        &ProviderConfig::from_document(document),
        &ProviderConfig::from_document(&written),
    )
    .map_err(|error| format!("the model providers refuse the change: {error}"))
}

/// Whether the providers may go from `previous` to `config`.
fn admitted(previous: &ProviderConfig, config: &ProviderConfig) -> Result<(), ModelError> {
    validate(config)?;
    for profile in &config.profiles {
        let old = previous.profiles.iter().find(|old| old.id == profile.id);
        let same_endpoint = old.is_some_and(|old| {
            old.endpoint == profile.endpoint
                && old.protocol == profile.protocol
                && old.location == profile.location
        });
        // Signed installation metadata is the provisioner's: an edit never mints a pinned
        // model, retargets one, or changes the models of a profile that holds one.
        let minted = profile.models.iter().any(|model| {
            model.revision.is_some()
                && !(same_endpoint && old.is_some_and(|old| old.models.contains(model)))
        });
        let retargeted = old.is_some_and(|old| {
            old.models.iter().any(|model| model.revision.is_some())
                && (!same_endpoint || old.models != profile.models)
        });
        if minted || retargeted {
            return Err(ModelError::new(ErrorKind::PermissionDenied));
        }
    }
    Ok(())
}

/// `config` with the revision an edit names it by: a digest of what it says.
fn revision(config: ProviderConfig) -> Result<ProviderSnapshot, ModelError> {
    use sha2::Digest as _;
    let bytes =
        serde_json::to_vec(&config).map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
    if bytes.len() > 256 * 1024 {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    Ok(ProviderSnapshot {
        config,
        revision: hex::encode(sha2::Sha256::digest(bytes)),
    })
}

fn bounded(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

/// Whether `config` stays within what a description may say: sizes, and no credential in an
/// endpoint.
fn validate(config: &ProviderConfig) -> Result<(), ModelError> {
    let invalid = || ModelError::new(ErrorKind::InvalidRequest);
    let bytes = serde_json::to_vec(config).map_err(|_| invalid())?;
    if bytes.len() > 256 * 1024
        || config.profiles.len() > 64
        || !config.validation_errors().is_empty()
        || config.agent_models.len() > 256
        || config.agent_models.keys().any(|agent| !bounded(agent, 256))
    {
        return Err(invalid());
    }
    for profile in &config.profiles {
        if !bounded(&profile.id, 128)
            || !bounded(&profile.label, 256)
            || !bounded(&profile.endpoint, 2048)
            || profile.models.len() > 256
            || profile
                .models
                .iter()
                .any(|model| !bounded(&model.id, 256) || !bounded(&model.label, 256))
        {
            return Err(invalid());
        }
        // Endpoint userinfo, queries and fragments can contain credentials.
        let url = url::Url::parse(&profile.endpoint).map_err(|_| invalid())?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
    }
    Ok(())
}
