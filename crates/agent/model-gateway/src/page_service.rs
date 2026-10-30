//! The model service a browser page runs for its own windows, in place of
//! the per-user gateway a Tilo computer runs.
//!
//! A page has no gateway process to hand its windows' requests to, so the
//! page's host keeps the service: windows are lent a [`PageModels`] (the
//! same [`ModelBackend`] a native window is lent) and Model Providers is lent
//! the [`PageService`] as its [`ProviderAdmin`], exactly as natively. Neither
//! can read a key back. The descriptions of the person's providers are their
//! settings document ([`ProviderConfig::load`]); each API key sits in the
//! person's own encrypted store ([`ProviderKeys`]), bound to its provider's
//! origin as the gateway binds it, and only this service reads it, to send
//! it to that provider.
//!
//! A provider that answers browsers itself (most hosted APIs do) is asked
//! directly. One that does not is asked through the fetch proxy of the
//! workspace host the page is admitted to, which relays the request and
//! keeps nothing; the service remembers which origins needed it.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_provider_config::management::{
    AdminFuture, CredentialStatus, ProviderAdmin, ProviderSnapshot,
};
use agent_provider_config::protocol::{
    CompletionFuture, ErrorKind, ModelBackend, ModelError, ModelRequest,
};
use agent_provider_config::{
    ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol, transform,
};
use tilo_net::proxy::ProxyUse;
use zeroize::Zeroizing;

use crate::conversation;

/// The longest a provider may take over one turn.
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest a provider may take to list its models.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// The most of an answer read.
const MAX_ANSWER_BYTES: usize = 8 * 1024 * 1024;
/// The most of a failure's body read, to classify it.
const MAX_FAILURE_BYTES: usize = 16 * 1024;
/// The longest API key accepted.
const MAX_KEY_BYTES: usize = 4096;

/// The person's own store of provider API keys, by [`key_slot`].
pub trait ProviderKeys: Send + Sync {
    /// The key in `slot`.
    ///
    /// # Errors
    /// The store cannot be read.
    fn get(&self, slot: &str) -> Result<Option<Zeroizing<String>>, ModelError>;
    /// Keep `key` in `slot`.
    ///
    /// # Errors
    /// The store refused it.
    fn put(&self, slot: &str, key: &str) -> Result<(), ModelError>;
    /// Forget the key in `slot`.
    ///
    /// # Errors
    /// The store refused it.
    fn delete(&self, slot: &str) -> Result<(), ModelError>;
}

/// Where `profile`'s key is kept: its id and the origin of its endpoint, so
/// a key typed for one provider is never sent to another one the profile was
/// later pointed at.
///
/// # Errors
/// An endpoint that is not an http(s) URL.
pub fn key_slot(profile: &ProviderProfile) -> Result<String, ModelError> {
    let url = tilo_net::Url::parse(&profile.endpoint)
        .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    Ok(format!(
        "provider-api-key:{}:{}",
        profile.id,
        url.origin().ascii_serialization()
    ))
}

/// The page's model service. Lend it to Model Providers as its
/// [`ProviderAdmin`], and its [`PageService::models`] to windows.
pub struct PageService {
    keys: Arc<dyn ProviderKeys>,
    client: tilo_net::Client,
    /// Origins whose provider refused the browser, asked through the proxy.
    proxied: Mutex<BTreeSet<String>>,
}

impl PageService {
    /// # Errors
    /// No HTTP client on this platform.
    pub fn new(keys: Arc<dyn ProviderKeys>) -> Result<Self, ModelError> {
        let client = tilo_net::Client::builder()
            .redirects(tilo_net::Redirects::None)
            .build()
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
        Ok(Self {
            keys,
            client,
            proxied: Mutex::default(),
        })
    }

    /// The models windows are lent: `choice` (a profile and one of its
    /// models), or the person's default when `None`.
    #[must_use]
    pub fn models(self: &Arc<Self>, choice: Option<(String, String)>) -> PageModels {
        PageModels {
            service: Arc::clone(self),
            choice,
        }
    }

    async fn complete(
        &self,
        choice: Option<&(String, String)>,
        request: &ModelRequest,
    ) -> Result<agent_provider_config::protocol::ModelResponse, ModelError> {
        let (profile, model) = select(&ProviderConfig::load(), choice)?;
        let key = self.key(&profile)?;
        let body = conversation::body(&profile, &model, request)?;
        let body =
            serde_json::to_vec(&body).map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
        let url = transform::url(profile.protocol, &profile.endpoint);
        let answer = self
            .exchange(
                &profile,
                key.as_deref().map(String::as_str),
                TURN_TIMEOUT,
                |client| {
                    client
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(body.clone())
                },
            )
            .await?;
        conversation::response(profile.protocol, &answer)
    }

    /// The key `profile` authenticates with, `None` for one that needs none.
    fn key(&self, profile: &ProviderProfile) -> Result<Option<Zeroizing<String>>, ModelError> {
        if profile.sign_in.is_some() {
            return Err(ModelError {
                kind: ErrorKind::Unsupported,
                message: format!(
                    "Signing in to {} works on a Tilo computer; in a browser, add an API key \
                     for it in Model Providers",
                    profile.display_label()
                ),
            });
        }
        if profile.credential.is_none() {
            return Ok(None);
        }
        match self.keys.get(&key_slot(profile)?)? {
            Some(key) => Ok(Some(key)),
            None => Err(ModelError {
                kind: ErrorKind::Authentication,
                message: format!(
                    "{} has no API key yet; add it in Model Providers",
                    profile.display_label()
                ),
            }),
        }
    }

    /// Send what `build` makes to `profile`'s provider and read its answer:
    /// directly, or through the host's proxy for a provider that refuses
    /// browsers.
    async fn exchange(
        &self,
        profile: &ProviderProfile,
        key: Option<&str>,
        timeout: Duration,
        build: impl Fn(&tilo_net::Client) -> tilo_net::RequestBuilder + Sync,
    ) -> Result<String, ModelError> {
        let origin = tilo_net::Url::parse(&profile.endpoint)
            .map(|url| url.origin().ascii_serialization())
            .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
        let headers = transform::profile_headers(profile, key)
            .map_err(|_| ModelError::new(ErrorKind::Authentication))?;
        let send = |route: ProxyUse| {
            let mut request = build(&self.client).timeout(timeout).fetch_proxy(route);
            for header in &headers {
                let Ok(mut value) = tilo_net::HeaderValue::from_str(&header.value) else {
                    return Err(ModelError::new(ErrorKind::Authentication));
                };
                value.set_sensitive(header.secret);
                request = request.header(header.name, value);
            }
            if profile.protocol == ProviderProtocol::Anthropic {
                // Anthropic answers a page only when it says it is one.
                request = request.header("anthropic-dangerous-direct-browser-access", "true");
            }
            Ok(request.send())
        };
        let proxied = self.proxied_origin(&origin);
        let route = if proxied {
            ProxyUse::CrossOrigin
        } else {
            ProxyUse::Never
        };
        let response = match send(route)?.await {
            Err(error) if !proxied && error.may_be_cors() => {
                // A provider that does not answer browsers fails their
                // preflight, so the request itself never left the page.
                let retried = send(ProxyUse::CrossOrigin)?.await;
                if retried.is_ok()
                    && let Ok(mut known) = self.proxied.lock()
                {
                    known.insert(origin);
                }
                retried
            }
            other => other,
        }
        .map_err(|error| {
            ModelError::new(if error.is_timeout() {
                ErrorKind::TimedOut
            } else {
                ErrorKind::Transport
            })
        })?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let detail = response
                .bytes_limited(MAX_FAILURE_BYTES)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            return Err(match status.as_u16() {
                404 | 405 => ModelError::new(ErrorKind::Unsupported),
                code => tilo_model_client::failure::Failure::from_http(
                    code,
                    &detail,
                    retry_after.as_deref(),
                )
                .model_error(),
            });
        }
        let bytes = response
            .bytes_limited(MAX_ANSWER_BYTES)
            .await
            .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn proxied_origin(&self, origin: &str) -> bool {
        self.proxied
            .lock()
            .is_ok_and(|known| known.contains(origin))
    }
}

/// The profile and model a request runs on: `choice`, or the person's
/// default provider and its default model.
fn select(
    config: &ProviderConfig,
    choice: Option<&(String, String)>,
) -> Result<(ProviderProfile, String), ModelError> {
    if !config.validation_errors().is_empty() {
        return Err(ModelError {
            kind: ErrorKind::InvalidRequest,
            message: "The model provider settings need fixing in Model Providers".into(),
        });
    }
    if let Some((profile_id, model_id)) = choice {
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == *profile_id && profile.enabled)
            .filter(|profile| profile.models.iter().any(|model| model.id == *model_id))
            .ok_or_else(|| ModelError::new(ErrorKind::NoProvider))?;
        return Ok((profile.clone(), model_id.clone()));
    }
    let profile = config
        .active_profile()
        .or_else(|| config.profiles.iter().find(|profile| profile.enabled))
        .ok_or_else(|| ModelError::new(ErrorKind::NoProvider))?;
    let model = profile
        .default_model
        .clone()
        .or_else(|| profile.models.first().map(|model| model.id.clone()))
        .ok_or_else(|| ModelError {
            kind: ErrorKind::NoProvider,
            message: format!("{} has no model chosen yet", profile.display_label()),
        })?;
    Ok((profile.clone(), model))
}

/// The revision a snapshot of `config` carries: its descriptions' digest.
fn snapshot(config: ProviderConfig) -> Result<ProviderSnapshot, ModelError> {
    let bytes =
        serde_json::to_vec(&config).map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
    if bytes.len() > 256 * 1024 {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    // FNV-1a: a change detector for compare-and-swap, not a security digest.
    let digest = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    Ok(ProviderSnapshot {
        config,
        revision: format!("{digest:016x}-{}", bytes.len()),
    })
}

fn profile(profile_id: &str) -> Result<ProviderProfile, ModelError> {
    ProviderConfig::load()
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| ModelError::new(ErrorKind::InvalidRequest))
}

fn unavailable(_: std::io::Error) -> ModelError {
    ModelError::new(ErrorKind::Unavailable)
}

impl ProviderAdmin for PageService {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async { snapshot(ProviderConfig::load()) })
    }

    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async move {
            if !config.validation_errors().is_empty() || config.profiles.len() > 64 {
                return Err(ModelError::new(ErrorKind::InvalidRequest));
            }
            let previous = ProviderConfig::load();
            if snapshot(previous.clone())?.revision != expected_revision {
                return Err(ModelError::new(ErrorKind::Conflict));
            }
            // A key belongs to its provider's origin. Removing a provider or
            // moving it elsewhere needs its key removed first, as natively.
            for old in &previous.profiles {
                let kept = config
                    .profiles
                    .iter()
                    .find(|new| new.id == old.id)
                    .is_some_and(|new| {
                        key_slot(new).ok() == key_slot(old).ok() && new.credential == old.credential
                    });
                if !kept && old.credential.is_some() && self.keys.get(&key_slot(old)?)?.is_some() {
                    return Err(ModelError::new(ErrorKind::PermissionDenied));
                }
            }
            if !config.compare_save(&previous).map_err(unavailable)? {
                return Err(ModelError::new(ErrorKind::Conflict));
            }
            snapshot(config)
        })
    }

    fn credential_status(&self, profile_id: String) -> AdminFuture<'_, CredentialStatus> {
        Box::pin(async move {
            let profile = profile(&profile_id)?;
            Ok(
                if profile.sign_in.is_none() && self.keys.get(&key_slot(&profile)?)?.is_some() {
                    CredentialStatus::Configured
                } else {
                    CredentialStatus::Missing
                },
            )
        })
    }

    fn list_models(&self, profile_id: String) -> AdminFuture<'_, Vec<ModelDescriptor>> {
        Box::pin(async move {
            let profile = profile(&profile_id)?;
            let key = self.key(&profile)?;
            let url = transform::models_url(&profile)
                .ok_or_else(|| ModelError::new(ErrorKind::Unsupported))?;
            let body = self
                .exchange(
                    &profile,
                    key.as_deref().map(String::as_str),
                    LIST_TIMEOUT,
                    |client| client.get(&url),
                )
                .await?;
            transform::parse_models(&body).map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
        })
    }

    fn set_api_key(&self, profile_id: String, api_key: String) -> AdminFuture<'_, ()> {
        let api_key = Zeroizing::new(api_key);
        Box::pin(async move {
            let profile = profile(&profile_id)?;
            let key = api_key.trim();
            if profile.credential.is_none()
                || key.is_empty()
                || key.len() > MAX_KEY_BYTES
                || key.chars().any(char::is_control)
            {
                return Err(ModelError::new(ErrorKind::InvalidRequest));
            }
            self.keys.put(&key_slot(&profile)?, key)
        })
    }

    fn remove_api_key(&self, profile_id: String) -> AdminFuture<'_, ()> {
        Box::pin(async move { self.keys.delete(&key_slot(&profile(&profile_id)?)?) })
    }

    fn migrate_api_key(&self, _: String, _: String) -> AdminFuture<'_, ()> {
        // A page never kept keys the old, unbound way: nothing to migrate.
        Box::pin(async { Err(ModelError::new(ErrorKind::Unsupported)) })
    }

    fn remove_legacy_api_key(&self, _: String) -> AdminFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A window's models: the page's service, for one choice or the default.
pub struct PageModels {
    service: Arc<PageService>,
    choice: Option<(String, String)>,
}

impl ModelBackend for PageModels {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        Box::pin(self.service.complete(self.choice.as_ref(), request))
    }
}

/// Keys in memory, for tests and a page without a store.
#[derive(Default)]
pub struct MemoryKeys(Mutex<std::collections::BTreeMap<String, Zeroizing<String>>>);

impl ProviderKeys for MemoryKeys {
    fn get(&self, slot: &str) -> Result<Option<Zeroizing<String>>, ModelError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?
            .get(slot)
            .cloned())
    }

    fn put(&self, slot: &str, key: &str) -> Result<(), ModelError> {
        self.0
            .lock()
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?
            .insert(slot.into(), Zeroizing::new(key.into()));
        Ok(())
    }

    fn delete(&self, slot: &str) -> Result<(), ModelError> {
        self.0
            .lock()
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))?
            .remove(slot);
        Ok(())
    }
}
