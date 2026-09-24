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
//! [`crate::application::HostContext`] can use its `services()` directly.

use std::cell::RefCell;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock, RwLock};

use crate::clipboard::{ClipboardBackend, InProcessClipboard};
use crate::mpris::{MprisCommand, MprisConfig, PlayerSnapshot};
use crate::portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome,
};

pub mod environment;
/// The native desktop implementation; see [`crate::native::services`].
#[cfg(feature = "native")]
pub use crate::native::services as native;
mod recording;

pub use environment::{
    AccessibilityInputs, HostEnvironment, HostIdentity, HostPaths, RunnerDiagnostics, environment,
    install_environment,
};
pub use recording::HostRecorder;

/// Host-provided file choosing and opening.
///
/// Every method returns a channel the UI thread drains once per frame, so an
/// implementation may answer immediately, from a worker thread, or from a
/// later host callback — it must never block the caller.
pub trait FilePicker: Send + Sync {
    /// A single-file Open, Save or folder chooser.
    fn pick(&self, request: PickRequest) -> PortalChannel;
    /// A multi-select Open chooser.
    fn pick_multiple(&self, request: PickRequest) -> MultiChannel;
    /// A single-file Open chooser that delivers the already-open descriptor.
    fn open_file(&self, request: PickRequest) -> OpenFileChannel;
    /// Open a local path with its default handler, or reveal its folder.
    fn launch(&self, request: LaunchRequest) -> LaunchChannel;
}

/// Hands a URI to whatever the host registers as its handler.
pub trait UriOpener: Send + Sync {
    /// Open `uri` without waiting for the handler to finish.
    ///
    /// # Errors
    /// The host refused or cannot open URIs at all.
    fn open(&self, uri: &str) -> std::io::Result<()>;
}

/// Creates the speech output the built-in screen reader narrates through.
pub trait SpeechFactory: Send + Sync {
    /// A fresh speaker. Must not fail: a host without speech returns a
    /// [`tts::NullSpeaker`].
    fn make(&self) -> Box<dyn tts::Speaker>;
}

/// One user-visible notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// A stable tag: showing the same tag again replaces the earlier one.
    pub tag: String,
    pub title: String,
    pub body: String,
}

/// Why a notification could not be shown or withdrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyError {
    /// This host has no notification service.
    Unavailable,
    /// The service rejected the request.
    Failed(String),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("notifications are unavailable in this host"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for NotifyError {}

/// Host-owned user notifications.
pub trait Notifier: Send + Sync {
    /// Show `notification`, replacing any earlier one with the same tag.
    ///
    /// # Errors
    /// The host has no notification service, or refused this one.
    fn show(&self, notification: &Notification) -> Result<(), NotifyError>;

    /// Withdraw the notification shown under `tag`, if any.
    ///
    /// # Errors
    /// The host has no notification service, or refused the request.
    fn withdraw(&self, tag: &str) -> Result<(), NotifyError>;
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
}

impl HostServices {
    /// Services that never leave the process: an in-process clipboard, file
    /// choosers that fail, no URI handler, silent speech, no notifications
    /// and no media controls.
    pub fn null() -> Self {
        Self {
            clipboard: Arc::new(InProcessClipboard::default()),
            files: Arc::new(UnavailableFilePicker),
            opener: Arc::new(UnavailableOpener),
            speech: Arc::new(SilentSpeech),
            notifications: Arc::new(UnavailableNotifier),
            media_session: Arc::new(NoMediaSession),
        }
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

const UNAVAILABLE: &str = "file choosing is unavailable in this host";

struct UnavailableFilePicker;

fn answered<T>(outcome: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

impl FilePicker for UnavailableFilePicker {
    fn pick(&self, _request: PickRequest) -> PortalChannel {
        answered(PortalOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn pick_multiple(&self, _request: PickRequest) -> MultiChannel {
        answered(MultiOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn open_file(&self, _request: PickRequest) -> OpenFileChannel {
        answered(OpenFileOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn launch(&self, _request: LaunchRequest) -> LaunchChannel {
        answered(LaunchOutcome::Failed(UNAVAILABLE.to_string()))
    }
}

struct UnavailableOpener;

impl UriOpener for UnavailableOpener {
    fn open(&self, _uri: &str) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "opening URIs is unavailable in this host",
        ))
    }
}

struct SilentSpeech;

impl SpeechFactory for SilentSpeech {
    fn make(&self) -> Box<dyn tts::Speaker> {
        Box::new(tts::NullSpeaker::new())
    }
}

pub(crate) struct UnavailableNotifier;

impl Notifier for UnavailableNotifier {
    fn show(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn withdraw(&self, _tag: &str) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }
}

struct NoMediaSession;

impl MediaSessionFactory for NoMediaSession {
    fn start(&self, _config: MprisConfig, _initial: PlayerSnapshot) -> Option<MediaSession> {
        None
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

/// The services effects on this thread should reach right now; see the
/// module docs for the resolution order.
pub fn current() -> HostServices {
    if let Some(scoped) = SCOPES.with(|scopes| scopes.borrow().last().cloned()) {
        return scoped;
    }
    if let Some(installed) = installed_default() {
        return installed;
    }
    static NULL: OnceLock<HostServices> = OnceLock::new();
    NULL.get_or_init(HostServices::null).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::ClipboardTarget;

    #[test]
    fn null_services_perform_nothing_outside_the_process() {
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

    #[test]
    fn scopes_nest_and_restore() {
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

    #[test]
    fn scope_is_restored_after_a_panic() {
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
