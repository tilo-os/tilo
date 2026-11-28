//! The session's `xdg-desktop-portal` backend, served under [`BUS_NAME`]: what
//! sandboxed and Flatpak apps ask the frontend for and their sandbox blocks — a
//! file, a screenshot, the appearance, keeping the screen awake, screen capture,
//! a remote desktop, global shortcuts, the clipboard and the frontend's own
//! permission store. The session-based ones live as
//! `org.freedesktop.impl.portal.Session` objects ([`session`]).
//! The backend has no UI: what needs the person's answer (which file, whether an
//! app may take a screenshot) is asked on the session's [`AskSender`](tilo_service::AskSender), and
//! what the person may act on is published as resources with actions ([`Portal`]).
//! Only the frontend may call the backend: every call is authenticated as coming
//! from the owner of `org.freedesktop.portal.Desktop` ([`auth`]), which has already
//! derived the app id from the app's credentials.

#![allow(
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    reason = "the interfaces mirror the portal's fixed D-Bus signatures: properties take the \
              object, methods take their arguments by value and name some they ignore"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tilo_permissions::caps::DeviceCap;

use serde_json::{Map, Value};
use tilo_actions::{ActionOutput, ApiError, ResourcePresentation, ResourceSpec};
use tilo_service::{AskSender, Service, Started, decode, offer, reading};
use zbus::zvariant::OwnedValue;

pub mod access;
pub mod account;
pub mod app_chooser;
pub mod appearance;
pub mod auth;
pub mod background;
pub mod capture_broker;
pub mod captures;
pub mod clipboard;
pub mod consent;
mod dispatch;
pub mod doc_store;
pub mod documents;
pub mod dynamic_launcher;
mod eis;
pub mod email;
pub mod file_chooser;
pub mod global_shortcuts;
pub mod host;
mod i18n;
pub mod inhibit;
pub mod input_capture;
pub mod lifecycle;
pub mod lockdown;
pub mod notification;
pub mod permission_store;
#[cfg(feature = "pipewire")]
pub mod pipewire;
pub mod print;
pub mod reach;
pub mod remote_desktop;
pub mod reply;
pub mod requester;
pub mod restore;
pub mod screencast;
pub mod screensaver;
pub mod screenshot;
pub mod secret;
pub mod session;
pub mod settings;
pub mod streams;
pub mod trigger;
pub mod usb;
pub mod wallpaper;

#[cfg(test)]
mod dbus_tests;
#[cfg(test)]
mod session_dbus_tests;

pub use auth::{AuthenticatedCall, PortalAuthenticator};
pub use consent::Consents;
pub use inhibit::{Inhibition, Inhibitions, SessionState};
pub use remote_desktop::{InputInjector, NoInput, RemoteInput};
pub use requester::Requesters;
pub use settings::{PortalSetting, Settings};
pub use streams::{CaptureSource, CaptureStreams, NoStreams, SourceKind, Stream, StreamRequest};

/// The bus name the backend serves under, as `tilo.portal` names it.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.tilo";
/// Where every backend interface lives.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// The resource the portal publishes what it holds under.
pub const PORTAL_RESOURCE: &str = "portal";
const CONSENT_PREFIX: &str = "portal:consent:";

/// The portal itself, as a declared type: everything it holds.
#[derive(tilo_resource::Row)]
#[row(kind = "portal", label = "Portal")]
pub struct PortalRoot;

pub use tilo_device_surfaces::privacy::{ConsentEffect, ConsentRow};

/// A kept answer's resource: `portal:consent:<device>\u{1f}<app>\u{1f}<capability>`.
#[must_use]
pub fn consent_resource(kept: &tilo_peer_state::DeviceAnswer) -> String {
    format!(
        "{CONSENT_PREFIX}{}\u{1f}{}\u{1f}{}",
        kept.device,
        kept.app,
        kept.cap.name()
    )
}

/// The app a request is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalCaller {
    /// The app id the frontend vouched for, or the launch named.
    pub app_id: String,
    /// The workspace the app runs in, when the session knows it.
    pub workspace: Option<String>,
    /// The app runs in one of the launcher's own sandboxes: its mount
    /// namespace holds nothing of the host it was not given, so a file the
    /// person chooses for it is answered with its reference in the session's
    /// table of choices ([`doc_store`]) rather than a host path it cannot open.
    pub sandboxed: bool,
    /// The portals the app's manifest lets it use (its `[sandbox.portals]`
    /// entries that are not denied), when it is a package its manifest confines:
    /// the backend refuses it every other portal ([`reach`]). `None` for an
    /// app no manifest confines.
    pub portals: Option<std::collections::BTreeSet<String>>,
}

impl PortalCaller {
    #[must_use]
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            workspace: None,
            sandboxed: false,
            portals: None,
        }
    }

    /// Whether the app may call backend interface `interface`: always when
    /// no manifest confines it, else only where [`reach::reaches`] says.
    #[must_use]
    pub fn reaches(&self, interface: &str) -> bool {
        self.portals
            .as_ref()
            .is_none_or(|allowed| reach::reaches(allowed, interface))
    }
}

/// A future the session answers on its own thread.
pub type Pending<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a screenshot is of, as the interface's `target` option names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureTarget {
    /// Everything the displays show.
    Screen,
    /// One window the person picks.
    Window,
    /// An area the person draws.
    Area,
    /// The window with the keyboard's focus.
    ActiveWindow,
}

impl CaptureTarget {
    /// The bit of the interface's `AvailableTargets` that stands for this
    /// target.
    #[must_use]
    pub const fn bit(self) -> u32 {
        match self {
            Self::Screen => 1,
            Self::Window => 2,
            Self::Area => 4,
            Self::ActiveWindow => 8,
        }
    }

    /// The target whose bit is `raw`, which is what the interface's
    /// `target` option holds; `None` for anything else.
    #[must_use]
    pub const fn of_bit(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::Screen,
            2 => Self::Window,
            4 => Self::Area,
            8 => Self::ActiveWindow,
            _ => return None,
        })
    }
}

/// Why what the session was asked for is not there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// Nothing was taken: the person closed the selection, or nobody waits
    /// for it any more. The app is answered as a cancel, not as a failure.
    Cancelled,
    /// What was asked for could not be captured.
    Failed(String),
}

/// What a colour pick answers when there is no pointer over a display to
/// take a colour from.
pub const NO_COLOUR_UNDER_POINTER: &str = "move the pointer over the colour to pick it";

/// What the session captures for the portal: the compositor's pictures of
/// what the displays show, and what the person picks or draws in the
/// studio's selector.
pub trait Capture: Send + Sync {
    /// A picture of `target` written as a PNG the asking app may read; its
    /// path. The targets the person takes part in are answered once they
    /// chose, however long that takes.
    fn screenshot(&self, target: CaptureTarget) -> Pending<Result<PathBuf, CaptureError>>;

    /// The colour under the pointer as red, green and blue, each from 0 to
    /// 1: the pixel of what the displays show there when it is asked.
    fn color_at_pointer(&self) -> Pending<Result<(f64, f64, f64), CaptureError>>;

    /// The targets this session can take, as the interface's
    /// `AvailableTargets` bits: it is asked nothing it cannot answer.
    fn targets(&self) -> u32;
}

/// A session that captures nothing.
pub struct NoCapture;

impl Capture for NoCapture {
    fn screenshot(&self, _target: CaptureTarget) -> Pending<Result<PathBuf, CaptureError>> {
        Box::pin(async {
            Err(CaptureError::Failed(
                "this session captures nothing".to_owned(),
            ))
        })
    }

    fn color_at_pointer(&self) -> Pending<Result<(f64, f64, f64), CaptureError>> {
        Box::pin(async {
            Err(CaptureError::Failed(
                "this session captures nothing".to_owned(),
            ))
        })
    }

    fn targets(&self) -> u32 {
        0
    }
}

/// Everything the backend's interfaces share.
#[derive(Clone)]
pub struct Portal {
    questions: AskSender,
    authenticator: PortalAuthenticator,
    capture: Arc<dyn Capture>,
    host: Arc<dyn host::Host>,
    app_states: background::AppStates,
    autostart: background::Autostart,
    consents: Consents,
    settings: Settings,
    inhibitions: Inhibitions,
    requests: lifecycle::Requests,
    sessions: session::Sessions,
    streams: Arc<dyn CaptureStreams>,
    input: Arc<dyn InputInjector>,
    captures: captures::Captures,
    grants: restore::Grants,
    permissions: permission_store::PermissionStore,
    casts: screencast::Casts,
    remotes: remote_desktop::Remotes,
    input_captures: input_capture::InputCaptures,
    shortcuts: global_shortcuts::Shortcuts,
    clipboards: clipboard::Clipboards,
    spooler: Arc<dyn print::Spooler>,
    chosen: std::sync::Arc<tilo_dialogs::chosen::ChosenTable>,
}

impl Portal {
    /// A backend asking through `questions`, taking calls only from the
    /// frontend `authenticator` vouches for, and giving apps the files the
    /// person chose through the session's one table, `chosen`
    /// ([`tilo_dialogs::chosen::chosen_table`]).
    /// Access answered beyond the session is kept once the session hands it
    /// the person's store ([`Self::with_consents`]).
    #[must_use]
    pub fn new(
        questions: AskSender,
        authenticator: PortalAuthenticator,
        chosen: std::sync::Arc<tilo_dialogs::chosen::ChosenTable>,
    ) -> Self {
        let consents = Consents::new(questions.clone(), tilo_peer_state::DeviceGrants::unkept());
        let sessions = session::Sessions::default();
        let remotes = remote_desktop::Remotes::default();
        remotes.follow(&sessions);
        let input_captures = input_capture::InputCaptures::default();
        input_captures.follow(&sessions);
        Self {
            questions,
            authenticator,
            capture: Arc::new(NoCapture),
            host: Arc::new(host::System),
            app_states: background::AppStates::default(),
            autostart: background::Autostart::default(),
            consents: consents.clone(),
            settings: Settings::default(),
            inhibitions: Inhibitions::default(),
            requests: lifecycle::Requests::default(),
            captures: captures::Captures::new(&sessions),
            grants: restore::Grants::default(),
            permissions: permission_store::PermissionStore::load(
                private_state_root().map(|root| root.join("tilo/portal/permissions.json")),
            )
            .answering(consents.grants().clone()),
            casts: screencast::Casts::default(),
            shortcuts: global_shortcuts::Shortcuts::new(&sessions),
            streams: Arc::new(NoStreams),
            input: Arc::new(NoInput),
            clipboards: clipboard::Clipboards::new(
                &sessions,
                remotes.clone(),
                input_captures.clone(),
            ),
            remotes,
            input_captures,
            sessions,
            spooler: Arc::new(print::Cups),
            chosen,
        }
    }

    /// The same, printing through `spooler`: a test's own, which records the jobs instead of
    /// sending them to this machine's queue.
    #[must_use]
    pub fn with_spooler(mut self, spooler: Arc<dyn print::Spooler>) -> Self {
        self.spooler = spooler;
        self
    }

    /// The same, sharing screens through `streams`.
    #[must_use]
    pub fn with_streams(mut self, streams: Arc<dyn CaptureStreams>) -> Self {
        self.streams = streams;
        self
    }

    /// The same, letting remote desktops drive `input`.
    #[must_use]
    pub fn with_input(mut self, input: Arc<dyn InputInjector>) -> Self {
        self.input = input;
        self
    }

    /// The same, sharing the desktop's selection through `clipboard`.
    #[must_use]
    pub fn with_clipboard(self, clipboard: Arc<dyn clipboard::Clipboard>) -> Self {
        self.clipboards.set(clipboard);
        self
    }

    /// The remote sessions' clipboards, told when the desktop's selection
    /// changes.
    #[must_use]
    pub const fn clipboards(&self) -> &clipboard::Clipboards {
        &self.clipboards
    }

    /// The same, keeping the frontend's permissions and restore data in
    /// `permissions`.
    #[must_use]
    pub fn with_permissions(mut self, permissions: permission_store::PermissionStore) -> Self {
        self.permissions = permissions;
        self
    }

    /// The files the person chose for an app: one table, which answers what
    /// the file chooser hands out ([`doc_store`]) and what the interfaces take
    /// in ([`documents`]). It is the session's table
    /// ([`tilo_dialogs::chosen::chosen_table`]), never one of the portal's own.
    #[must_use]
    pub const fn chosen(&self) -> &std::sync::Arc<tilo_dialogs::chosen::ChosenTable> {
        &self.chosen
    }

    /// The live portal sessions: where the session hears that what the
    /// portal publishes changed ([`session::Sessions::on_change`]).
    #[must_use]
    pub const fn sessions(&self) -> &session::Sessions {
        &self.sessions
    }

    /// What is captured now, and the broker revoking it on a screen lock
    /// or a privacy switch.
    #[must_use]
    pub const fn captures(&self) -> &captures::Captures {
        &self.captures
    }

    /// The input captures the session feeds its input and screens to.
    #[must_use]
    pub const fn input_captures(&self) -> &input_capture::InputCaptures {
        &self.input_captures
    }

    /// The apps' bound shortcuts, which the desktop's own table overrides.
    #[must_use]
    pub const fn shortcuts(&self) -> &global_shortcuts::Shortcuts {
        &self.shortcuts
    }

    /// The remembered shares.
    #[must_use]
    pub const fn grants(&self) -> &restore::Grants {
        &self.grants
    }

    fn controls(&self) -> remote_desktop::Controls {
        remote_desktop::Controls {
            sessions: self.sessions.clone(),
            remotes: self.remotes.clone(),
            captures: self.input_captures.clone(),
            casts: self.casts.clone(),
        }
    }

    /// The same, capturing through `capture`.
    #[must_use]
    pub fn with_capture(mut self, capture: Arc<dyn Capture>) -> Self {
        self.capture = capture;
        self
    }

    /// The same, acting on the device through `host`.
    #[must_use]
    pub fn with_host(mut self, host: Arc<dyn host::Host>) -> Self {
        self.host = host;
        self
    }

    /// The same, writing the autostart entries apps ask for through
    /// `autostart`.
    #[must_use]
    pub fn with_autostart(mut self, autostart: background::Autostart) -> Self {
        self.autostart = autostart;
        self
    }

    /// The apps running now, as the session tells the frontend.
    #[must_use]
    pub const fn app_states(&self) -> &background::AppStates {
        &self.app_states
    }

    /// The person's access decisions, which the portal checks its requests
    /// with: what every other service of the session that asks about a
    /// capability shares, so an answer given to one stands for the other.
    #[must_use]
    pub const fn consents(&self) -> &Consents {
        &self.consents
    }

    /// End every capture whose device `ended` says the app may no longer use, asked of each by
    /// its app, its workspace and the device: a stream it holds of the screen, the camera or the
    /// microphone, its input capture and the remote control of the desktop, and the shares it
    /// keeps a restore key for. The app keeps running. Returns how many captures ended.
    pub fn end_captures(
        &self,
        ended: impl Fn(&str, Option<&str>, DeviceCap) -> bool + Copy,
    ) -> usize {
        use capture_broker::CaptureKind;
        let streams = self.captures.keep_only(|app, workspace, kind| {
            let device = match kind {
                CaptureKind::Camera => DeviceCap::Camera,
                CaptureKind::Microphone => DeviceCap::Microphone,
                CaptureKind::Screen => DeviceCap::Screen,
                CaptureKind::SystemAudio => DeviceCap::SystemAudio,
            };
            !ended(
                app,
                Some(workspace).filter(|named| !named.is_empty()),
                device,
            )
        });
        let sessions = [
            session::SessionKind::InputCapture,
            session::SessionKind::RemoteDesktop,
        ]
        .into_iter()
        .flat_map(|kind| self.sessions.of_kind(kind))
        .filter(|(path, live)| {
            ended(
                &live.caller.app_id,
                live.caller.workspace.as_deref(),
                DeviceCap::Input,
            ) && self.sessions.end(path)
        })
        .count();
        // A remembered share is an answer of the same kind: once the app may no longer use
        // the screen or the input it shares, its restore key asks the person again.
        let forgotten = self.grants.forget_ended(|grant| {
            let workspace = grant.workspace.as_deref();
            (!grant.sources.is_empty() && ended(&grant.app_id, workspace, DeviceCap::Screen))
                || (grant.devices != 0 && ended(&grant.app_id, workspace, DeviceCap::Input))
        });
        if forgotten {
            self.sessions.changed();
        }
        streams.saturating_add(sessions)
    }

    /// Tell the frontend which of its device entries changed since it last heard, however they
    /// changed: the person's answers change on their other devices too.
    pub fn announce_device_answers(&self) {
        let Some(connection) = self.sessions.connection() else {
            return;
        };
        let store = self.permissions.clone();
        self.sessions.spawn(async move {
            permission_store::announce_devices(&connection, &store).await;
        });
    }

    /// The same, deciding device access with `consents`.
    #[must_use]
    pub fn with_consents(mut self, consents: Consents) -> Self {
        self.permissions = self.permissions.answering(consents.grants().clone());
        self.consents = consents;
        self
    }

    /// The same, taking the holds apps ask for in `registry`: the session's
    /// own, so its idle, lock and sleep decisions honour them like any other
    /// hold, and the person sees who keeps the machine awake.
    #[must_use]
    pub fn with_inhibitions(mut self, registry: tilo_power_mgmt::InhibitRegistry) -> Self {
        self.inhibitions = Inhibitions::in_registry(registry);
        self
    }

    /// The questions it asks through.
    #[must_use]
    pub const fn questions(&self) -> &AskSender {
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
        server
            .at(
                OBJECT_PATH,
                permission_store::PermissionStoreBackend::new(self.clone()),
            )
            .await?;
        server
            .at(OBJECT_PATH, app_chooser::AppChooser::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, account::Account::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, email::Email::new(self.clone()))
            .await?;
        server
            .at(
                OBJECT_PATH,
                dynamic_launcher::DynamicLauncher::new(self.clone(), connection.clone()),
            )
            .await?;
        server
            .at(OBJECT_PATH, wallpaper::Wallpaper::new(self.clone()))
            .await?;
        server
            .at(OBJECT_PATH, background::Background::new(self.clone()))
            .await?;
        screensaver::ScreenSaver::new(self.inhibitions.clone())
            .serve_on(connection)
            .await?;
        server
            .at(
                OBJECT_PATH,
                secret::Secret::on_session(self.clone(), connection.clone()),
            )
            .await?;
        server
            .at(
                OBJECT_PATH,
                print::Print::new(self.clone(), Arc::clone(&self.spooler)),
            )
            .await?;
        server.at(OBJECT_PATH, usb::Usb::new(self.clone())).await?;
        self.casts.follow(self);
        self.sessions.serve_on(connection);
        server
            .at(OBJECT_PATH, screencast::ScreenCast::new(self.clone()))
            .await?;
        server
            .at(
                OBJECT_PATH,
                remote_desktop::RemoteDesktop::new(self.clone()),
            )
            .await?;
        server
            .at(
                OBJECT_PATH,
                input_capture::InputCapture::new(self.clone(), connection),
            )
            .await?;
        server
            .at(
                OBJECT_PATH,
                global_shortcuts::GlobalShortcuts::new(self.clone()),
            )
            .await?;
        server
            .at(OBJECT_PATH, clipboard::ClipboardPortal::new(self.clone()))
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
        // Another desktop's screensaver may own its name; the portal serves
        // without it.
        if let Err(error) = connection.request_name(screensaver::BUS_NAME).await {
            tracing::warn!(%error, "another screensaver owns its name");
        }
        connection.request_name(BUS_NAME).await
    }

    /// Ask the person question `kind` for `caller` on behalf of request
    /// `path`, with `details` for the sheet.
    ///
    /// # Errors
    /// [`reply::Ended::Cancelled`] when they declined.
    async fn ask(
        &self,
        caller: &PortalCaller,
        path: &str,
        kind: &str,
        details: impl serde::Serialize,
    ) -> Result<tilo_service::Answer, reply::Ended> {
        self.questions
            .ask(tilo_service::Question {
                handle: question_handle(path),
                kind: kind.into(),
                app_id: Some(caller.app_id.clone()),
                label: caller.app_id.clone(),
                details: serde_json::to_value(details).unwrap_or_default(),
            })
            .await
            .ok_or(reply::Ended::Cancelled)
    }

    /// Authenticate a call that names no app (it acts on a request or a
    /// store the frontend owns) as the frontend's: its unique name.
    ///
    /// # Errors
    /// Access denied to anyone else.
    async fn frontend(
        &self,
        header: &zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<zbus::names::OwnedUniqueName> {
        let sender = header
            .sender()
            .cloned()
            .map(zbus::names::OwnedUniqueName::from);
        self.authenticator
            .authenticate_frontend(sender.as_ref())
            .await
            .map_err(|error| zbus::fdo::Error::AccessDenied(error.to_string()))
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
        let call = self
            .authenticator
            .authenticate_request(sender.as_ref(), handle, app_id, parent_window)
            .await
            .map_err(|error| {
                tracing::debug!(%error, app_id, "portal call refused");
                reply::Ended::Failed
            })?;
        // A package reaches only the portals its manifest lets it use, whatever reached the
        // frontend for it.
        let interface = header.interface().map(|name| name.as_str().to_owned());
        let reaches = match interface.as_deref() {
            Some(interface) => call.caller.reaches(interface),
            None => call.caller.portals.is_none(),
        };
        if !reaches {
            tracing::debug!(
                app_id = %call.caller.app_id,
                ?interface,
                "portal call refused: the app's manifest does not allow this portal"
            );
            return Err(reply::Ended::Failed);
        }
        Ok(call)
    }
}

/// What one part of the backend publishes: its resources under
/// [`PORTAL_RESOURCE`], their readings and their actions.
pub(crate) trait Published {
    fn resources(&self) -> Vec<ResourceSpec>;

    /// The reading of `resource`; `None` when it is not this part's.
    fn read(&self, resource: &str) -> Option<Value>;

    /// Carry out `action` of `resource`, given everything `published`;
    /// `None` when the resource is not this part's.
    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, ApiError>>;
}

impl Published for Consents {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.standing()
            .into_iter()
            .map(|kept| {
                offer::<ConsentEffect>(tilo_resource::typed_resource::<ConsentRow>(
                    consent_resource(&kept.answer),
                    kept.answer.app.clone(),
                ))
                .with_presentation(ResourcePresentation {
                    detail: Some(kept.answer.cap.name().to_owned()),
                    // An answer kept for another of the person's devices names it.
                    badge: (!kept.this_device).then(|| kept.device_label.clone()),
                    preview: None,
                })
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        resource.strip_prefix(CONSENT_PREFIX)?;
        self.standing()
            .into_iter()
            .find(|kept| consent_resource(&kept.answer) == resource)
            .and_then(|kept| serde_json::to_value(kept).ok())
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, ApiError>> {
        resource.strip_prefix(CONSENT_PREFIX)?;
        let unknown = || ApiError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        Some(
            decode(published, resource, action, arguments).and_then(|ConsentEffect::Forget| {
                let kept = self
                    .standing()
                    .into_iter()
                    .find(|kept| consent_resource(&kept.answer) == resource)
                    .ok_or_else(unknown)?;
                self.forget(&kept.answer.device, &kept.answer.app, kept.answer.cap)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(unknown)
            }),
        )
    }
}

impl Portal {
    /// Every part that publishes something; `controls` are the
    /// controlling sessions' ([`Portal::controls`]).
    fn parts<'a>(&'a self, controls: &'a remote_desktop::Controls) -> [&'a dyn Published; 7] {
        [
            &self.consents,
            &self.inhibitions,
            &self.autostart,
            &self.captures,
            controls,
            &self.grants,
            &self.shortcuts,
        ]
    }
}

impl Service for Portal {
    fn resources(&self) -> Vec<ResourceSpec> {
        let controls = self.controls();
        vec![
            tilo_resource::typed_resource::<PortalRoot>(
                PORTAL_RESOURCE,
                crate::i18n::t("portal-resource"),
            )
            .with_children(
                self.parts(&controls)
                    .into_iter()
                    .flat_map(Published::resources),
            ),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource == PORTAL_RESOURCE {
            return Some(serde_json::json!({
                "consents": self
                    .consents
                    .standing()
                    .into_iter()
                    .map(|kept| {
                        let mut read = reading(&kept);
                        if let Value::Object(fields) = &mut read {
                            fields.insert("resource".into(), consent_resource(&kept.answer).into());
                        }
                        read
                    })
                    .collect::<Vec<_>>(),
                "inhibitions": self.inhibitions.reading(),
                "autostart": self.autostart.apps(),
            }));
        }
        let controls = self.controls();
        self.parts(&controls)
            .into_iter()
            .find_map(|part| part.read(resource))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let published = self.resources();
        let controls = self.controls();
        self.parts(&controls)
            .into_iter()
            .find_map(|part| part.invoke(&published, resource, action, arguments))
            .unwrap_or_else(|| {
                Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                    resource, action,
                )))
            })
    }

    fn poll(&mut self) -> bool {
        self.sessions.take_changed()
    }
}

/// Option `key` of a portal call, as `T`.
pub(crate) fn option<T: TryFrom<OwnedValue>>(
    options: &HashMap<String, OwnedValue>,
    key: &str,
) -> Option<T> {
    T::try_from(options.get(key)?.try_clone().ok()?).ok()
}

/// Text option `key` of a portal call: at most `max` bytes, no control
/// characters, `None` when absent or not such text.
pub(crate) fn text_option(
    options: &HashMap<String, OwnedValue>,
    key: &str,
    max: usize,
) -> Option<String> {
    option::<String>(options, key).filter(|text| plain_text(text, max))
}

/// Whether `text` is at most `max` bytes with no control characters.
pub(crate) fn plain_text(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(char::is_control)
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
    /// frontend and keeps nothing, giving apps the files the person chose
    /// through `chosen`.
    pub(crate) fn portal_with(
        questions: &AskSender,
        chosen: std::sync::Arc<tilo_dialogs::chosen::ChosenTable>,
    ) -> Portal {
        let authenticator = PortalAuthenticator::fixed_for_tests(
            ":1.1".try_into().expect("unique name"),
            Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
        );
        Portal::new(questions.clone(), authenticator, chosen).with_consents(Consents::new(
            questions.clone(),
            tilo_peer_state::DeviceGrants::unkept(),
        ))
    }
}
