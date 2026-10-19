//! Host services — the capabilities a `ui` process borrows from whatever is
//! hosting it.
//!
//! Widgets and apps reach the clipboard, file choosers, URI handlers, speech,
//! notifications and media controls through [`HostServices`] instead of
//! calling a platform API directly. The same toolkit then runs under the
//! native desktop runner, inside a multi-app host such as the compositor, or
//! in a test that records every effect instead of performing it.
//!
//! # Resolution
//!
//! [`current`] answers, in order:
//!
//! 1. the innermost [`scoped`] set on this thread — a host running several
//!    apps in one process wraps each app's dispatch so its effects reach that
//!    app's services;
//! 2. the process default a runner installed with [`install_default`];
//! 3. [`HostServices::null`], which performs nothing outside the process.
//!
//! The existing free functions (`text_selection::clipboard_copy`,
//! `portal::PickRequest::send`, `widget::try_open_external_url`, …) resolve
//! through [`current`], so apps need no changes. New code that holds a
//! `ui::application::HostContext` can use its `services()` directly.

use std::cell::RefCell;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock, RwLock};

use crate::capture::{ScreenCaptureChannel, ScreenCaptureOutcome, ScreenCaptureRequest};
use crate::clipboard::{ClipboardBackend, InProcessClipboard};
use crate::mpris::{MprisCommand, MprisConfig, PlayerSnapshot};

pub mod environment;
/// The native desktop implementation; see [`crate::native::services`].
#[cfg(feature = "native")]
pub use crate::native::services as native;
mod recording;

pub use environment::{
    AccessibilityInputs, ContinuationMetadata, HostEnvironment, HostIdentity, HostPaths,
    RunnerDiagnostics, environment, install_environment, parse_posix_locale,
};
pub use recording::HostRecorder;

pub use tilo_dialogs::{FilePicker, UriOpener};

/// Creates the speech output the built-in screen reader narrates through.
pub trait SpeechFactory: Send + Sync {
    /// A fresh speaker. Must not fail: a host without speech returns a
    /// [`tts::NullSpeaker`].
    fn make(&self) -> Box<dyn tts::Speaker>;
}

pub use tilo_notify::{Notification, Notifier, NotifyError};

pub use tilo_location::LocationProvider;

pub use tilo_secrets::{SecretBackend, SecretStore};

/// Opens an app's secret store: the system's, or a host's own.
pub trait SecretsProvider: Send + Sync {
    /// The secrets of the app `app_id`, and no other app's.
    fn store(&self, app_id: &str) -> SecretStore;
}

/// Each app's secrets in this target's secret store (the Secret Service,
/// the Keychain, or the browser's wrapped store).
struct SystemSecrets;

impl SecretsProvider for SystemSecrets {
    fn store(&self, app_id: &str) -> SecretStore {
        SecretStore::for_app(app_id)
    }
}

/// Secrets in one map that lives as long as the provider, for hosts that
/// keep nothing outside the process.
pub struct InProcessSecrets(Arc<dyn SecretBackend>);

impl Default for InProcessSecrets {
    fn default() -> Self {
        Self(tilo_secrets::memory_backend())
    }
}

impl SecretsProvider for InProcessSecrets {
    fn store(&self, app_id: &str) -> SecretStore {
        SecretStore::with_backend(app_id, self.0.clone())
    }
}

/// Host-provided screen capture.
pub trait ScreenCaptureProvider: Send + Sync {
    /// Ask the user for a screen or window to capture. Answers over the
    /// channel, never blocking the caller while the host shows its picker.
    fn request(&self, request: ScreenCaptureRequest) -> ScreenCaptureChannel;
}

/// Receives player state for the host's media controls.
pub trait MediaSessionPublisher: Send {
    /// Publish the latest player state. Must not block.
    fn publish(&mut self, snapshot: PlayerSnapshot);
}

/// A live media-control session: commands from the host (media keys, a
/// now-playing widget) and the sink the player publishes its state into.
pub struct MediaSession {
    pub commands: Receiver<MprisCommand>,
    pub publisher: Box<dyn MediaSessionPublisher>,
}

/// Starts media-control sessions for players such as the `VideoPlayer` widget.
pub trait MediaSessionFactory: Send + Sync {
    /// Register a player described by `config`, starting from `initial`.
    /// `None` when the host has no media controls; the player then stays
    /// inert.
    fn start(&self, config: MprisConfig, initial: PlayerSnapshot) -> Option<MediaSession>;
}

/// Everything a `ui` process may ask of its host.
///
/// Cheap to clone: each service is shared.
#[derive(Clone)]
pub struct HostServices {
    pub clipboard: Arc<dyn ClipboardBackend>,
    pub files: Arc<dyn FilePicker>,
    pub opener: Arc<dyn UriOpener>,
    pub speech: Arc<dyn SpeechFactory>,
    pub notifications: Arc<dyn Notifier>,
    pub media_session: Arc<dyn MediaSessionFactory>,
    pub location: Arc<dyn LocationProvider>,
    pub screen_capture: Arc<dyn ScreenCaptureProvider>,
    pub secrets: Arc<dyn SecretsProvider>,
}

impl HostServices {
    /// Services that never leave the process: an in-process clipboard, file
    /// choosers that fail, no URI handler, silent speech, no notifications,
    /// no media controls, no location, no screen capture, and secrets kept
    /// in the process.
    pub fn null() -> Self {
        Self {
            clipboard: Arc::new(InProcessClipboard::default()),
            files: tilo_dialogs::unavailable_picker(),
            opener: tilo_dialogs::unavailable_opener(),
            speech: Arc::new(SilentSpeech),
            notifications: tilo_notify::unavailable(),
            media_session: Arc::new(NoMediaSession),
            location: tilo_location::unavailable(),
            screen_capture: Arc::new(NoScreenCapture),
            secrets: Arc::new(InProcessSecrets::default()),
        }
    }

    /// This target's services for the app `app_id`: each platform crate's
    /// system backend (clipboard, file choosers and URI opening,
    /// notifications, location, secrets) and, with the `native` feature, speech, media
    /// controls and screen capture. Without an app id notifications are
    /// unavailable.
    pub fn system(app_id: Option<&str>) -> Self {
        let services = Self {
            clipboard: tilo_clipboard::system(),
            files: tilo_dialogs::system_picker(),
            opener: tilo_dialogs::system_opener(),
            notifications: tilo_notify::system(app_id),
            location: tilo_location::system(),
            secrets: Arc::new(SystemSecrets),
            ..Self::null()
        };
        #[cfg(all(feature = "native", not(target_os = "android")))]
        let services = crate::native::services::with_native_backends(services);
        services
    }

    /// Replace the clipboard, keeping every other service.
    #[must_use]
    pub fn with_clipboard(mut self, clipboard: Arc<dyn ClipboardBackend>) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Replace file choosing and opening, keeping every other service.
    #[must_use]
    pub fn with_files(mut self, files: Arc<dyn FilePicker>) -> Self {
        self.files = files;
        self
    }
}

impl std::fmt::Debug for HostServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostServices")
            .field("clipboard", &self.clipboard.name())
            .finish_non_exhaustive()
    }
}

fn answered<T>(outcome: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

struct SilentSpeech;

impl SpeechFactory for SilentSpeech {
    fn make(&self) -> Box<dyn tts::Speaker> {
        Box::new(tts::NullSpeaker::new())
    }
}

struct NoMediaSession;

impl MediaSessionFactory for NoMediaSession {
    fn start(&self, _config: MprisConfig, _initial: PlayerSnapshot) -> Option<MediaSession> {
        None
    }
}

struct NoScreenCapture;

impl ScreenCaptureProvider for NoScreenCapture {
    fn request(&self, _request: ScreenCaptureRequest) -> ScreenCaptureChannel {
        answered(ScreenCaptureOutcome::Unavailable(
            "Screen capture isn't available here".to_string(),
        ))
    }
}

static DEFAULT: RwLock<Option<HostServices>> = RwLock::new(None);

thread_local! {
    static SCOPES: RefCell<Vec<HostServices>> = const { RefCell::new(Vec::new()) };
}

/// Install `services` as this process's default, replacing any earlier one.
///
/// A runner calls this once before constructing its app.
pub fn install_default(services: HostServices) {
    install_resolvers();
    if let Ok(mut slot) = DEFAULT.write() {
        tracing::debug!(
            target: "tilo_ui::host",
            clipboard = services.clipboard.name(),
            "host services installed"
        );
        *slot = Some(services);
    }
}

/// Install `make()` as the default unless one is already installed. Returns
/// whether it installed.
pub fn install_default_if_unset(make: impl FnOnce() -> HostServices) -> bool {
    install_resolvers();
    let Ok(mut slot) = DEFAULT.write() else {
        return false;
    };
    if slot.is_some() {
        return false;
    }
    *slot = Some(make());
    true
}

/// The installed default, if a runner installed one.
pub fn installed_default() -> Option<HostServices> {
    DEFAULT.read().ok()?.clone()
}

/// Run `f` with `services` as [`current`] on this thread.
///
/// Nests: the innermost scope wins, and the previous one is restored when `f`
/// returns or unwinds.
pub fn scoped<R>(services: &HostServices, f: impl FnOnce() -> R) -> R {
    let _scope = enter(services);
    f()
}

/// Make `services` [`current`] on this thread until the returned guard drops.
///
/// The guard form of [`scoped`], for a scope that spans several statements
/// (a test, a host's dispatch of one event). Guards must drop in reverse
/// order of creation, which Rust's scoping gives for free.
#[must_use = "the services are current only while the guard lives"]
pub fn enter(services: &HostServices) -> HostScope {
    install_resolvers();
    SCOPES.with(|scopes| scopes.borrow_mut().push(services.clone()));
    HostScope {
        _not_send: std::marker::PhantomData,
    }
}

/// Keeps a set of services [`current`] on this thread; see [`enter`].
pub struct HostScope {
    // The scope lives in a thread-local stack; dropping it elsewhere would
    // pop another thread's entry.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for HostScope {
    fn drop(&mut self) {
        SCOPES.with(|scopes| {
            scopes.borrow_mut().pop();
        });
    }
}

/// Let the platform crates' free functions (`portal::PickRequest::send`,
/// `portal::open_uri`, …) reach [`current`]. Runs before any services other
/// than [`HostServices::null`] can be current.
fn install_resolvers() {
    tilo_dialogs::install_resolver(tilo_dialogs::Resolver {
        picker: || current().files,
        opener: || current().opener,
    });
    tilo_location::install_resolver(|| current().location);
}

/// The services effects on this thread should reach right now; see the
/// module docs for the resolution order.
pub fn current() -> HostServices {
    static NULL: OnceLock<HostServices> = OnceLock::new();
    if let Some(scoped) = SCOPES.with(|scopes| scopes.borrow().last().cloned()) {
        return scoped;
    }
    if let Some(installed) = installed_default() {
        return installed;
    }
    NULL.get_or_init(HostServices::null).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardTarget;
    use crate::portal::{
        LaunchOutcome, LaunchRequest, MultiOutcome, OpenFileOutcome, PickRequest, PortalOutcome,
    };

    /// A request reaches whatever host is current: a recording host answers
    /// immediately, so a test never opens a chooser on the user's screen.
    #[test]
    fn requests_reach_the_current_host() {
        let (services, recorder) = HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened("/tmp/a.qvp".into()));
        scoped(&services, || {
            let single = PickRequest::open("Open", "Open").send();
            assert_eq!(
                single.try_recv(),
                Ok(PortalOutcome::Opened("/tmp/a.qvp".into()))
            );
            let save = PickRequest::save("Save", "Save", "x.qvp").send();
            assert_eq!(save.try_recv(), Ok(PortalOutcome::Cancelled));
            let many = PickRequest::open_multiple("Import", "Import").send_multiple();
            assert_eq!(many.try_recv(), Ok(MultiOutcome::Cancelled));
            let descriptor = PickRequest::open("Open", "Open").send_open_file();
            assert!(matches!(
                descriptor.try_recv(),
                Ok(OpenFileOutcome::Cancelled)
            ));
            let launch = LaunchRequest::reveal("/tmp/a.qvp").send();
            assert_eq!(launch.try_recv(), Ok(LaunchOutcome::Opened));
            assert!(crate::portal::open_uri("tilo://recorded").is_ok());
        });
        assert_eq!(recorder.pick_requests().len(), 4);
        assert_eq!(recorder.launch_requests().len(), 1);
        assert_eq!(recorder.opened_uris(), ["tilo://recorded"]);
    }

    #[test]
    fn host_services_are_inert_and_scopes_nest_and_restore_even_on_panic() {
        // Case `null_services_perform_nothing_outside_the_process`.
        {
            let null = HostServices::null();
            assert!(null.clipboard.set_text(ClipboardTarget::Clipboard, "local"));
            assert_eq!(
                null.clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                Some("local")
            );
            assert!(
                !null
                    .clipboard
                    .set_shared_text(ClipboardTarget::Clipboard, "secret"),
                "an in-process buffer must not claim a shared copy"
            );
            assert!(
                !null.clipboard.set_secret_text("secret"),
                "an in-process buffer must not claim a secret copy"
            );
            assert_eq!(
                null.opener
                    .open("https://example.invalid")
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Unsupported
            );
            assert!(matches!(
                null.files.pick(PickRequest::open("Open", "Open")).recv(),
                Ok(PortalOutcome::Failed(_))
            ));
            assert!(matches!(
                null.files.launch(LaunchRequest::open("/tmp/x")).recv(),
                Ok(LaunchOutcome::Failed(_))
            ));
            assert_eq!(
                null.notifications.withdraw("tag"),
                Err(NotifyError::Unavailable)
            );
            assert!(
                null.media_session
                    .start(MprisConfig::new("app", "App"), PlayerSnapshot::default())
                    .is_none()
            );
        }

        // Case `scopes_nest_and_restore`.
        {
            let (outer, outer_record) = HostServices::recording();
            let (inner, inner_record) = HostServices::recording();
            scoped(&outer, || {
                let _ = current().opener.open("outer://1");
                scoped(&inner, || {
                    let _ = current().opener.open("inner://1");
                });
                let _ = current().opener.open("outer://2");
            });
            assert_eq!(outer_record.opened_uris(), ["outer://1", "outer://2"]);
            assert_eq!(inner_record.opened_uris(), ["inner://1"]);
        }

        // Case `scope_is_restored_after_a_panic`.
        {
            let (services, record) = HostServices::recording();
            let depth = || SCOPES.with(|scopes| scopes.borrow().len());
            let before = depth();
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scoped(&services, || panic!("dispatch failed"));
            }));
            assert!(unwound.is_err());
            assert_eq!(depth(), before);
            assert!(record.opened_uris().is_empty());
        }
    }
}
