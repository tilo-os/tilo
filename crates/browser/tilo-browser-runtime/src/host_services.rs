//! The page's `HostServices`: each platform crate's web backend — the
//! clipboard of trusted clipboard events, `<input type=file>` choosers,
//! downloads, `window.open`, the Notifications API and the Geolocation API.
//! Speech is silent (`tts::NullSpeaker`); media controls and screen capture
//! are unavailable. The browser's reduced-motion and contrast preferences
//! are the page's accessibility inputs ([`crate::page_theme`]).
//!
//! Applications' notifications go to the desktop that shows their window,
//! as banners and in its notification centre
//! ([`tilo_desktop_surface::AppNotifier`]). The Notifications API shows
//! them too while the page is hidden and the person allows it, and on its
//! own for an application mounted without a desktop.
//!
//! Models are lent per mount, never ambient: the embedding host's model
//! service when it supplies a session, else the page's own
//! ([`tilo_model_gateway::page_service`]), which asks the providers the
//! person set up in Model Providers with the keys kept in this browser's
//! encrypted secret store. Only Model Providers is lent its administration.
//!
//! Browsers grant pickers, popups and permission prompts only inside a user
//! gesture. Every call comes from the app's handling of an input event, which
//! winit dispatches from the DOM listener, so it runs within that gesture's
//! activation.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::ProviderConfig;
use agent_provider_config::management::AdminFuture;
use tilo_model_gateway::page_service::ProviderKeys;
use ui::models::{
    CompletionFuture, CredentialStatus, ErrorKind, ModelBackend, ModelError, ModelRequest,
    ProviderAdmin, ProviderSnapshot, StreamSink,
};

/// Model access belongs to a mount, never to the page's ambient services.
/// The embedding host supplies a short-lived model-service session token,
/// not a provider credential. The service enforces its audience and grants.
/// A mount the host supplies none to is lent the page's own model service
/// ([`page_models`]).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelConfig {
    endpoint: String,
    session_token: String,
    profile_id: Option<String>,
    model_id: Option<String>,
}

impl ModelConfig {
    /// What a report's review calls this model: the one the embedding host
    /// chose, else its model service's default.
    pub(crate) fn name(&self) -> String {
        self.model_id
            .clone()
            .unwrap_or_else(|| "the model service's default model".into())
    }

    pub(crate) fn connect(&self) -> Result<ui::models::Models, String> {
        if self.session_token.trim().is_empty() {
            return Err("model session is required".into());
        }
        tilo_model_gateway::transport::check_endpoint(&self.endpoint)?;
        let choice = match (&self.profile_id, &self.model_id) {
            (None, None) => None,
            (Some(profile), Some(model))
                if !profile.trim().is_empty() && !model.trim().is_empty() =>
            {
                Some((profile.clone(), model.clone()))
            }
            _ => return Err("profileId and modelId must be supplied together and nonempty".into()),
        };
        Ok(ui::models::Models::new(Rc::new(LazyModels {
            service: Service::Host {
                endpoint: self.endpoint.clone(),
                session_token: zeroize::Zeroizing::new(self.session_token.clone()),
            },
            choice,
            client: std::cell::OnceCell::new(),
        })))
    }
}

/// A separate, explicitly granted administration session. Never derived from models.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderManagementConfig {
    endpoint: String,
    session_token: String,
}

impl ProviderManagementConfig {
    pub(crate) fn connect(&self) -> Result<ui::models::ProviderAdministration, String> {
        if self.session_token.trim().is_empty() {
            return Err("provider management session is required".into());
        }
        tilo_model_gateway::transport::check_endpoint(&self.endpoint)?;
        Ok(ui::models::ProviderAdministration::new(Arc::new(
            LazyAdministration {
                service: Service::Host {
                    endpoint: self.endpoint.clone(),
                    session_token: zeroize::Zeroizing::new(self.session_token.clone()),
                },
                client: std::sync::OnceLock::new(),
            },
        )))
    }
}

/// The page's own model service, for a mount its host lends none: the
/// providers the person set up in Model Providers, asked from this page
/// with the keys kept in the person's encrypted store in this browser.
pub(crate) fn page_models() -> ui::models::Models {
    ui::models::Models::new(Rc::new(LazyModels {
        service: Service::Page,
        choice: None,
        client: std::cell::OnceCell::new(),
    }))
}

/// Model Providers' administration of the page's own model service.
pub(crate) fn page_provider_admin() -> ui::models::ProviderAdministration {
    ui::models::ProviderAdministration::new(Arc::new(LazyAdministration {
        service: Service::Page,
        client: std::sync::OnceLock::new(),
    }))
}

/// Whether the page's own model service has a provider to ask: something
/// to lend a report's review rather than the room's model.
pub(crate) fn page_models_set_up() -> bool {
    ProviderConfig::load()
        .profiles
        .iter()
        .any(|profile| profile.enabled)
}

/// Which model service a mount's clients talk to.
enum Service {
    /// The embedding host's, over its session.
    Host {
        endpoint: String,
        session_token: zeroize::Zeroizing<String>,
    },
    /// The page's own.
    Page,
}

/// Where the page's model service keeps the person's provider keys: the
/// page's secret store (AES-GCM under a key the browser will not export,
/// in `IndexedDB`), under a name of its own that no application's store
/// reads.
struct PageKeys(tilo_secrets::SecretStore);

/// The secret store name the page's model service keeps keys under.
const PAGE_KEYS: &str = "so.tilo.model-service";

fn page_keys() -> Arc<dyn ProviderKeys> {
    Arc::new(PageKeys(tilo_secrets::SecretStore::for_app(PAGE_KEYS)))
}

fn key_store_error(_: tilo_secrets::SecretError) -> ModelError {
    ModelError {
        kind: ErrorKind::Unavailable,
        message: "This browser cannot keep provider keys; allow this site to store data".into(),
    }
}

impl ProviderKeys for PageKeys {
    fn get(&self, slot: &str) -> Result<Option<zeroize::Zeroizing<String>>, ModelError> {
        Ok(self
            .0
            .get_blocking(slot)
            .map_err(key_store_error)?
            .map(|secret| {
                zeroize::Zeroizing::new(String::from_utf8_lossy(secret.expose()).into_owned())
            }))
    }

    fn put(&self, slot: &str, key: &str) -> Result<(), ModelError> {
        self.0
            .put_blocking(slot, key.as_bytes(), tilo_secrets::PutOptions::default())
            .map_err(key_store_error)
    }

    fn delete(&self, slot: &str) -> Result<(), ModelError> {
        self.0.delete_blocking(slot).map_err(key_store_error)
    }
}

/// The model service's clients: the inference client and the provider
/// administration client, with the request and response codecs they carry.
///
/// They are linked in, unless the `deferred-model-clients` feature leaves
/// them to the bundling crate: the browser desktop hands over a loader
/// ([`defer_model_clients`]) that fetches them as their own module when a
/// window first asks a model or opens provider settings, so the start module
/// does not carry them.
#[derive(Clone, Copy)]
pub struct ModelClients {
    models: MakeModels,
    administration: MakeAdministration,
    page_models: MakePageModels,
    page_administration: MakePageAdministration,
}

/// Starts the page's own model service over the person's keys.
type MakePageModels = fn(Arc<dyn ProviderKeys>) -> Result<Rc<dyn ModelBackend>, ModelError>;

/// Starts the page's own model service for Model Providers.
type MakePageAdministration =
    fn(Arc<dyn ProviderKeys>) -> Result<Arc<dyn ProviderAdmin>, ModelError>;

/// Opens the model service client for a base URL, a token and an optional
/// provider credential pair.
type MakeModels =
    fn(&str, String, Option<(String, String)>) -> Result<Rc<dyn ModelBackend>, String>;

/// Opens the provider administration client for a base URL and a token.
type MakeAdministration = fn(&str, String) -> Result<Arc<dyn ProviderAdmin>, String>;

/// Hands over [`ModelClients`], for [`install_model_clients`].
pub type MakeModelClients = fn() -> ModelClients;

/// The model service's clients over the page's `fetch`.
#[must_use]
pub fn builtin_model_clients() -> ModelClients {
    fn models(
        endpoint: &str,
        session_token: String,
        choice: Option<(String, String)>,
    ) -> Result<Rc<dyn ModelBackend>, String> {
        use tilo_model_gateway::transport::{FetchTransport, ModelClient};
        let transport = FetchTransport::new(endpoint, session_token)?;
        let timeout = std::time::Duration::from_secs(120);
        Ok(Rc::new(match choice {
            None => ModelClient::default_model(transport, timeout),
            Some((profile, model)) => ModelClient::new(transport, profile, model, timeout),
        }))
    }
    fn administration(
        endpoint: &str,
        session_token: String,
    ) -> Result<Arc<dyn ProviderAdmin>, String> {
        use tilo_model_gateway::transport::{AdministrationClient, FetchTransport};
        let transport = FetchTransport::new(endpoint, session_token)?;
        Ok(Arc::new(AdministrationClient::new(transport)))
    }
    fn page_models(keys: Arc<dyn ProviderKeys>) -> Result<Rc<dyn ModelBackend>, ModelError> {
        let service = Arc::new(tilo_model_gateway::page_service::PageService::new(keys)?);
        Ok(Rc::new(service.models(None)))
    }
    fn page_administration(
        keys: Arc<dyn ProviderKeys>,
    ) -> Result<Arc<dyn ProviderAdmin>, ModelError> {
        Ok(Arc::new(
            tilo_model_gateway::page_service::PageService::new(keys)?,
        ))
    }
    ModelClients {
        models,
        administration,
        page_models,
        page_administration,
    }
}

#[cfg(feature = "deferred-model-clients")]
static MODEL_CLIENTS: ui::deferred::Deferred<ModelClients> = ui::deferred::Deferred::new();

/// Fetch the model service's clients with `load` the first time a window
/// uses them, instead of linking them in (the `deferred-model-clients`
/// feature). `load` starts the fetch and then calls [`install_model_clients`]
/// or [`model_clients_load_failed`].
#[cfg(feature = "deferred-model-clients")]
pub fn defer_model_clients(load: fn()) {
    MODEL_CLIENTS.defer(load);
}

/// The model service's clients arrived.
#[cfg(feature = "deferred-model-clients")]
pub fn install_model_clients(make: MakeModelClients) {
    MODEL_CLIENTS.install(make());
}

/// Fetching the model service's clients failed: the requests waiting for
/// them fail, and the next one fetches again.
#[cfg(feature = "deferred-model-clients")]
pub fn model_clients_load_failed() {
    MODEL_CLIENTS.load_failed();
}

#[cfg(feature = "deferred-model-clients")]
async fn model_clients() -> Result<ModelClients, ModelError> {
    MODEL_CLIENTS
        .wait()
        .await
        .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))
}

#[cfg(not(feature = "deferred-model-clients"))]
#[expect(
    clippy::unused_async,
    reason = "the deferred-model-clients build waits for the clients here"
)]
async fn model_clients() -> Result<ModelClients, ModelError> {
    Ok(builtin_model_clients())
}

/// A mount's models, connected on the first request.
struct LazyModels {
    service: Service,
    choice: Option<(String, String)>,
    client: std::cell::OnceCell<Rc<dyn ModelBackend>>,
}

impl LazyModels {
    async fn client(&self) -> Result<Rc<dyn ModelBackend>, ModelError> {
        if let Some(client) = self.client.get() {
            return Ok(Rc::clone(client));
        }
        let clients = model_clients().await?;
        let client = match &self.service {
            Service::Host {
                endpoint,
                session_token,
            } => (clients.models)(endpoint, session_token.to_string(), self.choice.clone())
                .map_err(|_| ModelError::new(ErrorKind::Transport))?,
            Service::Page => (clients.page_models)(page_keys())?,
        };
        Ok(Rc::clone(self.client.get_or_init(|| client)))
    }
}

impl ModelBackend for LazyModels {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        Box::pin(async move { self.client().await?.complete(request).await })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        Box::pin(async move { self.client().await?.stream(request, sink).await })
    }
}

/// A mount's provider administration, connected on the first request.
struct LazyAdministration {
    service: Service,
    client: std::sync::OnceLock<Arc<dyn ProviderAdmin>>,
}

impl LazyAdministration {
    async fn client(&self) -> Result<Arc<dyn ProviderAdmin>, ModelError> {
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        let clients = model_clients().await?;
        let client = match &self.service {
            Service::Host {
                endpoint,
                session_token,
            } => (clients.administration)(endpoint, session_token.to_string())
                .map_err(|_| ModelError::new(ErrorKind::Transport))?,
            Service::Page => (clients.page_administration)(page_keys())?,
        };
        Ok(Arc::clone(self.client.get_or_init(|| client)))
    }
}

impl ProviderAdmin for LazyAdministration {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async move { self.client().await?.get_config().await })
    }

    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async move {
            self.client()
                .await?
                .set_config(expected_revision, config)
                .await
        })
    }

    fn credential_status(&self, profile_id: String) -> AdminFuture<'_, CredentialStatus> {
        Box::pin(async move { self.client().await?.credential_status(profile_id).await })
    }

    fn list_models(
        &self,
        profile_id: String,
    ) -> AdminFuture<'_, Vec<agent_provider_config::ModelDescriptor>> {
        Box::pin(async move { self.client().await?.list_models(profile_id).await })
    }

    fn set_api_key(&self, profile_id: String, api_key: String) -> AdminFuture<'_, ()> {
        Box::pin(async move { self.client().await?.set_api_key(profile_id, api_key).await })
    }

    fn remove_api_key(&self, profile_id: String) -> AdminFuture<'_, ()> {
        Box::pin(async move { self.client().await?.remove_api_key(profile_id).await })
    }

    fn migrate_api_key(
        &self,
        profile_id: String,
        original_endpoint: String,
    ) -> AdminFuture<'_, ()> {
        Box::pin(async move {
            self.client()
                .await?
                .migrate_api_key(profile_id, original_endpoint)
                .await
        })
    }

    fn remove_legacy_api_key(&self, profile_id: String) -> AdminFuture<'_, ()> {
        Box::pin(async move { self.client().await?.remove_legacy_api_key(profile_id).await })
    }
}

/// Only the exact registered control panel may borrow administration: the
/// page runs Model Providers, never Settings (a Tilo OS app).
pub(crate) fn provider_admin_app(app_id: &str) -> bool {
    app_id == "so.tilo.model-providers"
}

/// Match trusted application metadata, not an arbitrary tab's label or requested alias.
pub(crate) fn provide_provider_admin(
    context: &mut ui::application::HostContext,
    handle: ui::models::ProviderAdministration,
) -> Result<(), String> {
    if handle.is_available() && !provider_admin_app(context.app_id()) {
        return Err(
            "provider administration is only available to Settings or Model Providers".into(),
        );
    }
    context.provide_provider_admin(handle);
    Ok(())
}

use tilo_desktop_surface::{AppInbox, AppNotifier};

thread_local! {
    static NOTIFIER: RefCell<Option<Arc<AppNotifier>>> = const { RefCell::new(None) };
    static BACKGROUND: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
}

/// Install the page's services as the process default. `wake` schedules
/// every desktop's next frame.
pub fn install(wake: Arc<dyn Fn() + Send + Sync>) {
    crate::page_theme::install(Arc::clone(&wake));
    let system = ui::host::HostServices::system(None);
    let notifier = Arc::new(AppNotifier::new(
        Arc::clone(&system.notifications),
        BACKGROUND.with(Arc::clone),
        wake,
    ));
    NOTIFIER.with_borrow_mut(|slot| *slot = Some(Arc::clone(&notifier)));
    ui::host::install_default(ui::host::HostServices {
        notifications: notifier,
        ..system
    });
}

/// A desktop's inbox of applications' notifications.
pub fn notification_inbox() -> Option<AppInbox> {
    NOTIFIER.with_borrow(|notifier| notifier.as_ref().map(|notifier| notifier.inbox()))
}

/// The page went out of view or came back. Hidden, applications'
/// notifications also go to the browser's, when the person allowed them.
pub fn set_hidden(hidden: bool) {
    let forward = hidden && notification_permission().as_deref() == Some("granted");
    BACKGROUND.with(|background| background.store(forward, Ordering::Release));
}

/// `Notification.permission`, where the page has the API.
fn notification_permission() -> Option<String> {
    let api = js_sys::Reflect::get(&js_sys::global(), &"Notification".into()).ok()?;
    if api.is_undefined() {
        return None;
    }
    js_sys::Reflect::get(&api, &"permission".into())
        .ok()?
        .as_string()
}
