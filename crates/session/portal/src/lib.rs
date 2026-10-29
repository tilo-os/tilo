//! The session's `xdg-desktop-portal` backend.
//!
//! Sandboxed and Flatpak apps ask `xdg-desktop-portal` (the frontend) for
//! what their sandbox blocks: a file, a screenshot, the desktop's
//! appearance, keeping the screen awake. The frontend forwards each request
//! to a backend; this crate is Tilo's, served under [`BUS_NAME`]:
//!
//! - `org.freedesktop.impl.portal.FileChooser` ([`file_chooser`])
//! - `org.freedesktop.impl.portal.Screenshot` ([`screenshot`])
//! - `org.freedesktop.impl.portal.Settings` ([`settings`], projecting the
//!   person's appearance for legacy toolkits, [`appearance`])
//! - `org.freedesktop.impl.portal.Access` ([`access`])
//! - `org.freedesktop.impl.portal.Inhibit` ([`inhibit`])
//! - `org.freedesktop.impl.portal.Lockdown` ([`lockdown`])
//!
//! The backend has no UI. What needs the person's answer (which file,
//! whether an app may take a screenshot) is asked through the session's
//! [`Questions`], which the session's sheet presents; what the backend
//! holds that the person may act on (the inhibitions in force, the access
//! they allowed for good) is published as resources with actions
//! ([`Portal`] is a [`Service`]).
//!
//! Only the frontend may call the backend: every call is authenticated as
//! coming from the owner of `org.freedesktop.portal.Desktop` ([`auth`]),
//! which has already derived the app id from the app's credentials.

#![allow(
    clippy::unused_self,
    clippy::used_underscore_binding,
    reason = "the interfaces mirror the portal's fixed D-Bus signatures: properties take the \
              object and methods name arguments they ignore"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_resource::Effect;
use tilo_service::{Questions, Service, Started, decode, offer, reading};
use zbus::zvariant::OwnedValue;

pub mod access;
pub mod appearance;
pub mod auth;
pub mod consent;
pub mod file_chooser;
pub mod inhibit;
pub mod lifecycle;
pub mod lockdown;
pub mod reply;
pub mod screenshot;
pub mod settings;

#[cfg(test)]
mod dbus_tests;

pub use auth::{AuthenticatedCall, PortalAuthenticator};
pub use consent::Consents;
pub use inhibit::{Inhibition, Inhibitions};
pub use settings::{PortalSetting, Settings};

/// The bus name the backend serves under, as `tilo.portal` names it.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.tilo";
/// Where every backend interface lives.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// The resource the portal publishes what it holds under.
pub const PORTAL_RESOURCE: &str = "portal";
const CONSENT_PREFIX: &str = "portal:consent:";

/// What a remembered access decision offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.consent")]
pub enum ConsentEffect {
    /// Forget what the app was allowed
    #[effect(own_device, idempotent)]
    Forget,
}

fn consent_resource(app: &str, capability: &str) -> String {
    format!("{CONSENT_PREFIX}{app}\u{1f}{capability}")
}

/// The app a request is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalCaller {
    /// The app id the frontend vouched for, or the launch named.
    pub app_id: String,
    /// The workspace the app runs in, when the session knows it.
    pub workspace: Option<String>,
}

impl PortalCaller {
    #[must_use]
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            workspace: None,
        }
    }
}

/// A future the session answers on its own thread.
pub type Pending<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the session captures for the portal: the compositor's pictures of
/// what the displays show.
pub trait Capture: Send + Sync {
    /// A picture of every display as they show now, written as a PNG the
    /// asking app may read; its path.
    fn screenshot(&self) -> Pending<Result<PathBuf, String>>;
}

/// A session that captures nothing.
pub struct NoCapture;

impl Capture for NoCapture {
    fn screenshot(&self) -> Pending<Result<PathBuf, String>> {
        Box::pin(async { Err("this session captures nothing".to_owned()) })
    }
}

/// Everything the backend's interfaces share.
#[derive(Clone)]
pub struct Portal {
    questions: Questions,
    authenticator: PortalAuthenticator,
    capture: Arc<dyn Capture>,
    consents: Consents,
    settings: Settings,
    inhibitions: Inhibitions,
    requests: lifecycle::Requests,
}

impl Portal {
    /// A backend asking through `questions`, taking calls only from the
    /// frontend `authenticator` vouches for. Access answered for good is
    /// kept in the person's private state.
    #[must_use]
    pub fn new(questions: Questions, authenticator: PortalAuthenticator) -> Self {
        let consents = Consents::new(
            questions.clone(),
            private_state_root().map(|root| root.join("tilo/portal/consents.json")),
        );
        Self {
            questions,
            authenticator,
            capture: Arc::new(NoCapture),
            consents,
            settings: Settings::default(),
            inhibitions: Inhibitions::default(),
            requests: lifecycle::Requests::default(),
        }
    }

    /// The same, capturing through `capture`.
    #[must_use]
    pub fn with_capture(mut self, capture: Arc<dyn Capture>) -> Self {
        self.capture = capture;
        self
    }

    /// The same, keeping access answered for good in `consents`.
    #[must_use]
    pub fn with_consents(mut self, consents: Consents) -> Self {
        self.consents = consents;
        self
    }

    /// The questions it asks through.
    #[must_use]
    pub const fn questions(&self) -> &Questions {
        &self.questions
    }

    /// The settings it serves, kept current by [`settings::follow_appearance`].
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// What apps keep the session from doing now.
    #[must_use]
    pub const fn inhibitions(&self) -> &Inhibitions {
        &self.inhibitions
    }

    /// Serve every interface on `connection`, without claiming a name.
    ///
    /// # Errors
    /// The object server refused an interface.
    pub async fn serve_on(&self, connection: &zbus::Connection) -> zbus::Result<()> {
        let server = connection.object_server();
        server
            .at(OBJECT_PATH, file_chooser::FileChooser::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, screenshot::Screenshot::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, settings::SettingsPortal::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, access::Access::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, inhibit::Inhibit::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, lockdown::LockdownBackend::for_session())
            .await?;
        self.requests.close_with_their_owners(connection);
        settings::emit_changes(connection, self.settings.subscribe());
        Ok(())
    }

    /// Serve every interface on `connection` and claim [`BUS_NAME`].
    ///
    /// # Errors
    /// The bus refused an interface or the name.
    pub async fn serve(&self, connection: &zbus::Connection) -> zbus::Result<()> {
        self.serve_on(connection).await?;
        connection.request_name(BUS_NAME).await
    }

    /// Authenticate a call carrying request `handle`, for `app_id`.
    async fn authenticate(
        &self,
        header: &zbus::message::Header<'_>,
        handle: &str,
        app_id: &str,
        parent_window: Option<&str>,
    ) -> Result<AuthenticatedCall, reply::Ended> {
        let sender = header
            .sender()
            .cloned()
            .map(zbus::names::OwnedUniqueName::from);
        self.authenticator
            .authenticate_request(sender.as_ref(), handle, app_id, parent_window)
            .await
            .map_err(|error| {
                tracing::debug!(%error, app_id, "portal call refused");
                reply::Ended::Failed
            })
    }
}

impl Service for Portal {
    fn resources(&self) -> Vec<ResourceSpec> {
        let consents = self
            .consents
            .remembered()
            .into_iter()
            .map(|(app, capability)| {
                offer::<ConsentEffect>(ResourceSpec::new(
                    consent_resource(&app, &capability),
                    "portal.consent",
                    app,
                ))
                .with_presentation(ResourcePresentation {
                    detail: Some(capability),
                    badge: None,
                    preview: None,
                })
            });
        vec![
            ResourceSpec::new(PORTAL_RESOURCE, "portal", "Portal")
                .with_children(consents)
                .with_children(self.inhibitions.resources()),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if let Some(consent) = resource.strip_prefix(CONSENT_PREFIX) {
            let (app, capability) = consent.split_once('\u{1f}')?;
            return self
                .consents
                .remembered()
                .contains(&(app.to_owned(), capability.to_owned()))
                .then(|| serde_json::json!({ "app_id": app, "capability": capability }));
        }
        if resource == PORTAL_RESOURCE {
            return Some(serde_json::json!({
                "consents": reading(&self.consents.remembered()),
                "inhibitions": self.inhibitions.reading(),
            }));
        }
        self.inhibitions.read(resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let unknown = || DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        let published = self.resources();
        if let Some(consent) = resource.strip_prefix(CONSENT_PREFIX) {
            let ConsentEffect::Forget = decode(&published, resource, action, arguments)?;
            let (app, capability) = consent.split_once('\u{1f}').ok_or_else(unknown)?;
            return self
                .consents
                .forget(app, capability)
                .then(|| Started::Done(ActionOutput::default()))
                .ok_or_else(unknown);
        }
        self.inhibitions
            .invoke(&published, resource, action, arguments)
            .unwrap_or_else(|| Err(unknown()))
    }
}

/// Option `key` of a portal call, as `T`.
pub(crate) fn option<T: TryFrom<OwnedValue>>(
    options: &HashMap<String, OwnedValue>,
    key: &str,
) -> Option<T> {
    T::try_from(options.get(key)?.try_clone().ok()?).ok()
}

/// A user-private persistent root. Never a predictable shared directory:
/// without a per-user state or runtime directory, nothing is kept.
pub(crate) fn private_state_root() -> Option<PathBuf> {
    tilo_paths::state_dir().or_else(tilo_paths::runtime_dir)
}

/// The question handle of portal request `path`.
pub(crate) fn question_handle(path: &str) -> String {
    format!("portal:{path}")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::*;

    /// A backend asking through `questions` that trusts `:1.1` as its
    /// frontend and keeps nothing.
    pub(crate) fn portal(questions: &Questions) -> Portal {
        let authenticator = PortalAuthenticator::fixed_for_tests(
            ":1.1".try_into().expect("unique name"),
            Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
        );
        Portal::new(questions.clone(), authenticator)
            .with_consents(Consents::new(questions.clone(), None))
    }
}
