//! The page's `ProcessServices`: each platform crate's web backend — the
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
//! service when it supplies a session, else the person's own model on their
//! server, asked with the key their vault keeps there
//! ([`server_models`]). A browser keeps no provider key. Only Model
//! Providers is lent an administration, and only one the embedder
//! grants.
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
use agent_provider_config::protocol::{ModelResponse, StreamEvent};
use tilo_actions::remote::person_models;
use ui::models::{
    CompletionFuture, CredentialStatus, ErrorKind, ModelBackend, ModelError, ModelRequest,
    ProviderAdmin, ProviderSnapshot, StreamSink,
};

/// Model access belongs to a mount, never to the page's ambient services.
/// The embedding host supplies a short-lived model-service session token,
/// not a provider credential. The service enforces its audience and grants.
/// A mount the embedder supplies none to asks the person's own model on their
/// server ([`server_models`]).
#[derive(Clone, serde::Deserialize)]
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
        tilo_model_gateway::transport::check_endpoint_in_page(&self.endpoint)?;
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
            service: EmbedderSession {
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
        tilo_model_gateway::transport::check_endpoint_in_page(&self.endpoint)?;
        Ok(ui::models::ProviderAdministration::new(Arc::new(
            LazyAdministration {
                service: EmbedderSession {
                    endpoint: self.endpoint.clone(),
                    session_token: zeroize::Zeroizing::new(self.session_token.clone()),
                },
                client: std::sync::OnceLock::new(),
            },
        )))
    }
}

/// The person's own models, for a mount its embedder lends none: the
/// server the page's peer `peer` is with asks them
/// ([`person_models::ASK`]), with the key the person's vault keeps there.
/// That is the model and the key Settings shows and their hosted agents use;
/// a browser keeps no provider key of its own. A page with no server has no
/// model to ask.
pub(crate) fn server_models(
    peer: Option<u32>,
    workspace: &str,
    person: &str,
) -> ui::models::Models {
    let Some(peer) = peer else {
        return ui::models::Models::default();
    };
    ui::models::Models::new(Rc::new(ServerTurns {
        peer,
        workspace: workspace.to_owned(),
        person: person.to_owned(),
    }))
}

/// The longest a turn of the person's model is followed.
const TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// Turns of the person's model, asked of the server over the
/// session the page's peer holds with it.
struct ServerTurns {
    peer: u32,
    workspace: String,
    person: String,
}

impl ServerTurns {
    /// One turn of the person's model, its answer told to `sink` as the
    /// server keeps what it wrote: the ask's items each hold the answer so
    /// far, and what this page has not shown yet of the newest one is its
    /// next delta. Its output is the whole turn.
    async fn turn(
        &self,
        request: &ModelRequest,
        sink: &mut dyn FnMut(StreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        use tilo_actions::remote::{
            ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, UpdateMark,
            settle_with,
        };
        let unreachable = || ModelError {
            kind: ErrorKind::Unavailable,
            message: "Your workspace's server can't be reached from this page right now".into(),
        };
        let workspace = crate::peer::workspace_id(&self.workspace).map_err(|_| unreachable())?;
        let api = crate::peer_sync::actions_of(self.peer, workspace).ok_or_else(unreachable)?;
        let serde_json::Value::Object(argument) = serde_json::to_value(request)
            .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?
        else {
            return Err(ModelError::new(ErrorKind::InvalidRequest));
        };
        let caller = Caller::local_person(self.person.clone()).in_workspace(self.workspace.clone());
        let invocation = tilo_actions::InvocationId::new();
        let asked = InvokeRequest {
            invocation,
            target: person_models::target(person_models::ASK),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let refused = |message: String| ModelError {
            kind: ErrorKind::Unavailable,
            message,
        };
        let mut receipt = api
            .invoke(&caller, asked)
            .await
            .ok_or_else(unreachable)?
            .map_err(|error| refused(error.to_string()))?;
        let now = tilo_clock::now();
        let until = now.checked_add(TURN_TIMEOUT).unwrap_or(now);
        let mut seen = UpdateMark::Accepted;
        let mut shown = Written::default();
        while matches!(receipt.status, ReceiptStatus::Accepted) {
            if tilo_clock::now() >= until {
                return Err(ModelError::new(ErrorKind::TimedOut));
            }
            let state = settle_with(&api, &caller, invocation, Some(seen), until)
                .await
                .flatten()
                .ok_or_else(unreachable)?;
            seen = UpdateMark::of(&state.update);
            if let tilo_actions::InvocationUpdate::Item { value, .. } = &state.update {
                shown.show(value, sink);
            }
            receipt = state.receipt();
        }
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => output,
            ReceiptStatus::Accepted => return Err(ModelError::new(ErrorKind::TimedOut)),
            ReceiptStatus::Cancelled { .. } => return Err(ModelError::new(ErrorKind::Cancelled)),
            ReceiptStatus::Failed { error } => return Err(refused(error.message)),
            ReceiptStatus::Rejected { error } => return Err(refused(error.to_string())),
        };
        serde_json::from_value::<Result<ModelResponse, ModelError>>(
            output.value.unwrap_or_default(),
        )
        .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))?
    }
}

/// How much of an ask's answer so far a page has shown.
#[derive(Default)]
struct Written {
    text: usize,
    reasoning: usize,
}

impl Written {
    /// Tell `sink` what the answer so far `value` adds to what was shown.
    fn show(&mut self, value: &serde_json::Value, sink: &mut dyn FnMut(StreamEvent)) {
        let newer = |key: &str, shown: &mut usize| {
            let text = value.get(key).and_then(serde_json::Value::as_str)?;
            let more = text
                .get(*shown..)
                .filter(|more| !more.is_empty())?
                .to_owned();
            *shown = text.len();
            Some(more)
        };
        if let Some(text) = newer("reasoning", &mut self.reasoning) {
            sink(StreamEvent::ReasoningDelta { text });
        }
        if let Some(text) = newer("text", &mut self.text) {
            sink(StreamEvent::TextDelta { text });
        }
    }
}

impl ModelBackend for ServerTurns {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a> {
        Box::pin(async move { self.turn(request, &mut |_| {}).await })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        mut sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        Box::pin(async move { self.turn(request, &mut sink).await })
    }
}

/// The embedder's model service, over the session it lent the mount.
struct EmbedderSession {
    endpoint: String,
    session_token: zeroize::Zeroizing<String>,
}

/// The model service's clients: the inference client and the provider
/// administration client, with the request and response codecs they carry.
///
/// They are linked in, unless the `deferred-model-clients` feature leaves
/// them to the bundling crate: the browser desktop fetches them
/// ([`MODEL_CLIENTS`]) as their own module when a
/// window first asks a model or opens provider settings, so the start module
/// does not carry them.
#[derive(Clone, Copy)]
pub struct ModelClients {
    models: MakeModels,
    administration: MakeAdministration,
}

/// Opens the model service client for a base URL, a token and an optional
/// provider credential pair.
type MakeModels =
    fn(&str, String, Option<(String, String)>) -> Result<Rc<dyn ModelBackend>, String>;

/// Opens the provider administration client for a base URL and a token.
type MakeAdministration = fn(&str, String) -> Result<Arc<dyn ProviderAdmin>, String>;

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
    ModelClients {
        models,
        administration,
    }
}

ui::lazy_code! {
    /// The model service's clients, fetched the first time a window uses
    /// them where the bundle defers them (`deferred-model-clients`).
    pub static MODEL_CLIENTS: ModelClients = builtin_model_clients, feature = "deferred-model-clients";
}

async fn model_clients() -> Result<ModelClients, ModelError> {
    MODEL_CLIENTS
        .wait()
        .await
        .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))
}

/// A mount's models, connected on the first request.
struct LazyModels {
    service: EmbedderSession,
    choice: Option<(String, String)>,
    client: std::cell::OnceCell<Rc<dyn ModelBackend>>,
}

impl LazyModels {
    async fn client(&self) -> Result<Rc<dyn ModelBackend>, ModelError> {
        if let Some(client) = self.client.get() {
            return Ok(Rc::clone(client));
        }
        let clients = model_clients().await?;
        let EmbedderSession {
            endpoint,
            session_token,
        } = &self.service;
        let client = (clients.models)(endpoint, session_token.to_string(), self.choice.clone())
            .map_err(|_| ModelError::new(ErrorKind::Transport))?;
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
    service: EmbedderSession,
    client: std::sync::OnceLock<Arc<dyn ProviderAdmin>>,
}

impl LazyAdministration {
    async fn client(&self) -> Result<Arc<dyn ProviderAdmin>, ModelError> {
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        let clients = model_clients().await?;
        let EmbedderSession {
            endpoint,
            session_token,
        } = &self.service;
        let client = (clients.administration)(endpoint, session_token.to_string())
            .map_err(|_| ModelError::new(ErrorKind::Transport))?;
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
}

/// Lend administration to the window only when it is the exact registered
/// control panel, matched by trusted application metadata, never a tab's
/// label or requested alias: the page runs Model Providers, never Settings (a
/// Tilo OS app).
pub(crate) fn provide_provider_admin(
    context: &mut ui::application::ProcessContext,
    handle: ui::models::ProviderAdministration,
) {
    if context.app_id() == "so.tilo.model-providers" {
        context.provide_provider_admin(handle);
    }
}

use tilo_desktop_surface::{AppInbox, AppNotifier};
use tilo_session::platform::notice::Toolkit;

thread_local! {
    static NOTIFIER: RefCell<Option<Arc<AppNotifier>>> = const { RefCell::new(None) };
    static BACKGROUND: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    /// The browser's notifications themselves, beside the desktop's: what the page tells the
    /// person directly while they are elsewhere.
    static BROWSER: RefCell<Option<Arc<dyn ui::notify::Notifier>>> = const { RefCell::new(None) };
    /// The request queues of the desktops this page shows, by mount: where a
    /// link an app opens lands when an app here claims it, so the page shows
    /// it in that app's window ([`crate::shell::HostRequest::OpenLink`]).
    static DESKTOP_REQUESTS: RefCell<std::collections::BTreeMap<u64, crate::shell::HostRequests>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
    /// The page's next frame: a request an app made outside an input event
    /// (a link the browser hands over later) has no frame of its own, so it
    /// asks for one; the desktop reads the request there.
    static WAKE: RefCell<Option<Arc<dyn Fn() + Send + Sync>>> = const { RefCell::new(None) };
}

/// Where a link an app on this page claims lands: the requests of the
/// desktop the page shows under `mount`, or nowhere when it shows one
/// application alone (which has no other app to hand it to).
pub(crate) fn serve_links(mount: u64, requests: Option<crate::shell::HostRequests>) {
    DESKTOP_REQUESTS.with_borrow_mut(|by_mount| match requests {
        Some(requests) => {
            by_mount.insert(mount, requests);
        }
        None => {
            by_mount.remove(&mount);
        }
    });
}

/// The page closed the mount `mount`: its links land nowhere.
pub(crate) fn stop_serving_links(mount: u64) {
    DESKTOP_REQUESTS.with_borrow_mut(|by_mount| {
        by_mount.remove(&mount);
    });
}

/// Show what `link` names in the app on this page that claims its scheme
/// (`ui::application::registry::link_claim`), through the session as a notice's
/// choice is: the app's `open-link`, in its window or one the session opens.
/// Whether an app claimed it and a desktop was here to show it.
fn open_in_app(link: &str) -> bool {
    let Some(claim) = ui::application::registry::link_claim(link) else {
        return false;
    };
    let request = crate::shell::HostRequest::OpenLink {
        app: claim.app_id.to_owned(),
        resource: claim.resource.to_owned(),
        link: link.to_owned(),
    };
    let shown = DESKTOP_REQUESTS.with_borrow(|by_mount| {
        let Some(requests) = by_mount.values().next() else {
            web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                "link {link}: no desktop"
            )));
            return false;
        };
        requests.borrow_mut().push(request);
        true
    });
    if shown && let Some(wake) = WAKE.with_borrow(Clone::clone) {
        wake();
    }
    shown
}

/// The page's URI opener: a link of a scheme an app on this page claims shows
/// in that app's window, as it does on a device; every other link is the
/// browser's.
struct PageOpener {
    elsewhere: Arc<dyn ui::process::UriOpener>,
}

impl ui::process::UriOpener for PageOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        if open_in_app(uri) {
            return Ok(());
        }
        self.elsewhere.open(uri)
    }
}

/// Install the page's services as the process default. `wake` schedules
/// every desktop's next frame.
pub fn install(wake: Arc<dyn Fn() + Send + Sync>) {
    crate::page_theme::install(Arc::clone(&wake));
    WAKE.with_borrow_mut(|slot| *slot = Some(Arc::clone(&wake)));
    let system = ui::process::ProcessServices::system(None);
    BROWSER.with_borrow_mut(|slot| *slot = Some(Arc::clone(&system.notifications)));
    let notifier = Arc::new(AppNotifier::new(
        Arc::clone(&system.notifications),
        BACKGROUND.with(Arc::clone),
        wake,
    ));
    NOTIFIER.with_borrow_mut(|slot| *slot = Some(Arc::clone(&notifier)));
    let opener: Arc<dyn ui::process::UriOpener> = Arc::new(PageOpener {
        elsewhere: Arc::clone(&system.opener),
    });
    ui::process::install_default(ui::process::ProcessServices {
        notifications: notifier,
        opener,
        ..system
    });
}

thread_local! {
    static NOTICES: std::cell::OnceCell<Rc<Toolkit>> = const { std::cell::OnceCell::new() };
}

/// What the session's roles tell the person on this page: one for the page, so the place of a
/// notice they choose is reported once.
pub(crate) fn notices() -> Rc<Toolkit> {
    let made = || Rc::new(Toolkit::new(ui::process::current().notifications, ""));
    NOTICES.with(|notices| Rc::clone(notices.get_or_init(made)))
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

/// Show `notification` in the browser's notifications, only while the page
/// is out of view and the person allows them: the shell tells them of it
/// itself otherwise.
pub(crate) fn tell_browser(notification: &ui::notify::Notification) {
    if !notifies_browser() {
        return;
    }
    let browser = BROWSER.with_borrow(Clone::clone);
    if let Some(Err(error)) = browser.map(|browser| browser.show(notification)) {
        tracing::debug!(%error, "no browser notification");
    }
}

/// Whether notifications go to the browser's now: the page is out of view
/// and the person allows them.
pub(crate) fn notifies_browser() -> bool {
    BACKGROUND.with(|background| background.load(Ordering::Acquire))
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
