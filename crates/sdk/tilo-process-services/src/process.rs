//! Process services — the capabilities an app borrows from its process.
//!
//! Widgets and apps reach the clipboard, file choosers, URI handlers, speech,
//! notifications and media controls through [`ProcessServices`] instead of
//! calling a platform API directly. The same toolkit then runs under the
//! native desktop runner, inside an app process such as the compositor's, or
//! in a test that records every effect instead of performing it.
//!
//! # Resolution
//!
//! [`current`] answers, in order:
//!
//! 1. the innermost [`scoped`] set on this thread — a process running several
//!    apps wraps each app's dispatch so its effects reach that app's services;
//! 2. the default an app process installed with [`install_default`];
//! 3. [`ProcessServices::null`], which performs nothing outside the process.
//!
//! The existing free functions (`text_selection::clipboard_copy`,
//! `portal::PickRequest::send`, `ui::platform::try_os_open`, …) resolve
//! through [`current`], so apps need no changes. New code that holds a
//! `ui::application::ProcessContext` can use its `services()` directly.

use std::cell::RefCell;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock, RwLock};

use crate::capture::{ScreenCaptureChannel, ScreenCaptureOutcome, ScreenCaptureRequest};
use crate::clipboard::{ClipboardBackend, InProcessClipboard};
use crate::mpris::{MprisCommand, MprisConfig, PlayerSnapshot};

/// The native desktop implementation; see [`crate::native::services`].
#[cfg(feature = "native")]
pub use crate::native::services as native;
/// Process inputs: who the process is and where its files live, kept with the
/// current process (`tilo_process::environment`).
pub use tilo_process::environment;
mod recording;

pub use environment::{
    AccessibilityInputs, ContinuationMetadata, NO_WINDOW_ENV, NO_WINDOW_IDLE_ENV,
    ProcessEnvironment, ProcessIdentity, ProcessPaths, REVEAL_ENV, RunnerDiagnostics, environment,
    install_environment, parse_posix_locale,
};
pub use recording::ProcessRecorder;

pub use tilo_dialogs::{FilePicker, UriOpener};

/// Creates the speech output the built-in screen reader narrates through.
pub trait SpeechFactory: Send + Sync {
    /// A fresh speaker. Must not fail: a process without speech returns a
    /// [`tts::NullSpeaker`].
    fn make(&self) -> Box<dyn tts::Speaker>;
}

pub use tilo_notify::{Notification, Notifier, NotifyError};

pub use crate::location::LocationProvider;

pub use tilo_secrets::{SecretBackend, SecretStore};

/// Opens an app's secret store: the system's, or a process's own.
pub trait SecretsProvider: Send + Sync {
    /// The secrets of the app `app_id`, and no other app's.
    fn store(&self, app_id: &str) -> SecretStore;
}

/// Each app's secrets in this target's secret store: the workspace's vault for a process its
/// launcher handed a connection to the daemon, else the Secret Service, the Keychain, or the
/// browser's wrapped store.
struct SystemSecrets;

impl SecretsProvider for SystemSecrets {
    fn store(&self, app_id: &str) -> SecretStore {
        #[cfg(target_os = "linux")]
        if let Some(vault) = handed_vault() {
            return SecretStore::with_backend(app_id, vault);
        }
        SecretStore::for_app(app_id)
    }
}

/// The workspace's vault over the connection the launcher handed this process, made once: `None`
/// for a process it handed none.
#[cfg(target_os = "linux")]
fn handed_vault() -> Option<Arc<dyn SecretBackend>> {
    static VAULT: std::sync::OnceLock<Option<Arc<dyn SecretBackend>>> = std::sync::OnceLock::new();
    VAULT
        .get_or_init(|| {
            crate::vault_secrets::VaultStore::handed()
                .map(|vault| Arc::new(vault) as Arc<dyn SecretBackend>)
        })
        .clone()
}

/// Secrets in one map that lives as long as the provider, for processes that
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

/// Process-provided screen capture.
pub trait ScreenCaptureProvider: Send + Sync {
    /// Ask the user for a screen or window to capture. Answers over the
    /// channel, never blocking the caller while the process shows its picker.
    fn request(&self, request: ScreenCaptureRequest) -> ScreenCaptureChannel;
}

/// Receives player state for the process's media controls.
pub trait MediaSessionPublisher: Send {
    /// Publish the latest player state. Must not block.
    fn publish(&mut self, snapshot: PlayerSnapshot);
}

/// A live media-control session: commands from the process (media keys, a
/// now-playing widget) and the sink the player publishes its state into.
pub struct MediaSession {
    pub commands: Receiver<MprisCommand>,
    pub publisher: Box<dyn MediaSessionPublisher>,
}

/// Starts media-control sessions for players such as the `VideoPlayer` widget.
pub trait MediaSessionFactory: Send + Sync {
    /// Register a player described by `config`, starting from `initial`.
    /// `None` when the process has no media controls; the player then stays
    /// inert.
    fn start(&self, config: MprisConfig, initial: PlayerSnapshot) -> Option<MediaSession>;
}

/// Everything a `ui` process may ask of its process.
///
/// Cheap to clone: each service is shared.
#[derive(Clone)]
pub struct ProcessServices {
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

impl ProcessServices {
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
            location: crate::location::unavailable(),
            screen_capture: Arc::new(NoScreenCapture),
            secrets: Arc::new(InProcessSecrets::default()),
        }
    }

    /// This target's services for the app `app_id`: the clipboard over the connection its
    /// session serves it, each platform crate's system backend (file choosers and URI opening,
    /// notifications, secrets) and, with the `native` feature, speech, media controls and screen
    /// capture. Location is the device's own action, which the host installs with
    /// [`Self::with_location`]; a process given none answers that location is not available.
    /// Without an app id notifications are unavailable.
    pub fn system(app_id: Option<&str>) -> Self {
        let services = Self {
            clipboard: tilo_clipboard::system(),
            files: tilo_dialogs::unavailable_picker(),
            opener: tilo_dialogs::system_opener(),
            notifications: tilo_notify::system(app_id),
            location: crate::location::unavailable(),
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

    /// Replace location, keeping every other service: the host installs one that asks the
    /// device's own action, so a sandboxed process never reaches the platform's location
    /// service itself.
    #[must_use]
    pub fn with_location(mut self, location: Arc<dyn LocationProvider>) -> Self {
        self.location = location;
        self
    }
}

impl std::fmt::Debug for ProcessServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessServices")
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

static DEFAULT: RwLock<Option<ProcessServices>> = RwLock::new(None);

thread_local! {
    static SCOPES: RefCell<Vec<ProcessServices>> = const { RefCell::new(Vec::new()) };
}

/// Install `services` as this process's default, replacing any earlier one.
///
/// A runner calls this once before constructing its app.
pub fn install_default(services: ProcessServices) {
    install_resolvers();
    if let Ok(mut slot) = DEFAULT.write() {
        tracing::debug!(
            target: "tilo_ui::process",
            clipboard = services.clipboard.name(),
            "process services installed"
        );
        *slot = Some(services);
    }
}

/// Install `make()` as the default unless one is already installed. Returns
/// whether it installed.
pub fn install_default_if_unset(make: impl FnOnce() -> ProcessServices) -> bool {
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

pub fn installed_default() -> Option<ProcessServices> {
    DEFAULT.read().ok()?.clone()
}

/// Run `f` with `services` as [`current`] on this thread.
///
/// Nests: the innermost scope wins, and the previous one is restored when `f`
/// returns or unwinds.
pub fn scoped<R>(services: &ProcessServices, f: impl FnOnce() -> R) -> R {
    let _scope = enter(services);
    f()
}

/// Make `services` [`current`] on this thread until the returned guard drops.
///
/// The guard form of [`scoped`], for a scope that spans several statements
/// (a test, a process's dispatch of one event). Guards must drop in reverse
/// order of creation, which Rust's scoping gives for free.
#[must_use = "the services are current only while the guard lives"]
pub fn enter(services: &ProcessServices) -> ProcessScope {
    install_resolvers();
    SCOPES.with(|scopes| scopes.borrow_mut().push(services.clone()));
    ProcessScope {
        _not_send: std::marker::PhantomData,
    }
}

/// Keeps a set of services [`current`] on this thread; see [`enter`].
pub struct ProcessScope {
    // The scope lives in a thread-local stack; dropping it elsewhere would
    // pop another thread's entry.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for ProcessScope {
    fn drop(&mut self) {
        SCOPES.with(|scopes| {
            scopes.borrow_mut().pop();
        });
    }
}

/// Let the platform crates' free functions (`portal::PickRequest::send`,
/// `portal::open_uri`, …) reach [`current`]. Runs before any services other
/// than [`ProcessServices::null`] can be current.
fn install_resolvers() {
    tilo_dialogs::install_resolver(tilo_dialogs::Resolver {
        picker: || current().files,
        opener: || current().opener,
    });
}

/// The services effects on this thread should reach right now; see the
/// module docs for the resolution order.
pub fn current() -> ProcessServices {
    static NULL: OnceLock<ProcessServices> = OnceLock::new();
    if let Some(scoped) = SCOPES.with(|scopes| scopes.borrow().last().cloned()) {
        return scoped;
    }
    if let Some(installed) = installed_default() {
        return installed;
    }
    NULL.get_or_init(ProcessServices::null).clone()
}
