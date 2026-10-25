//! Engine-agnostic embedded web view plumbing.
//!
//! The [`crate::widget::WebView`] widget displays frames produced by a
//! [`WebViewEngine`] backend and forwards input back into it. The engine
//! runs on its own thread (or entirely synchronously, for tests): commands
//! flow in through [`WebViewEngine::send`], rendered pixels come out
//! through a shared [`FrameSlot`], and navigation/UI state changes come
//! out as [`WebViewEvent`]s drained by the host app during `sync`.
//!
//! Backends:
//!  * [`test_engine::TestEngine`] — deterministic, dependency-free engine
//!    used by unit tests and `--shots` headless renders.
//!  * [`chromium::ChromiumEngine`] — the Tilo system engine: a Chromium
//!    (CEF) host process rendering real pages offscreen, delivered as its
//!    own signed runtime (see [`chromium::runtime`]).
//!
//! Coordinates in commands are **physical pixels relative to the web
//! view's top-left corner** — the widget does the logical→physical
//! translation so engines never see toolkit layout space.

pub mod chromium;
mod credentials;
mod service_sessions;
pub use credentials::{
    AddressFields, CredentialAccount, CredentialAction, CredentialForm, CredentialReply,
    CredentialRequest,
};
pub use service_sessions::{ServiceSessionReply, ServiceSessionRequest};
pub mod page;
pub mod paint;
pub mod text_input;
pub use page::{FrameId, PageFailure, PageKey, PageReply, PageRequest};
pub mod profile;
pub mod test_engine;

#[cfg(feature = "webview-adblock")]
pub mod adblock;

pub(crate) mod frame_trace;
mod scroll_marker;

use std::sync::{Arc, Mutex};

/// One rendered frame from the engine: tightly-packed RGBA8 at physical
/// pixel resolution. The buffer is shared, never copied per draw — the
/// widget detects staleness via the allocation pointer.
#[derive(Clone)]
pub struct Frame {
    /// Opt-in Rust receipt identity; never implies input causality.
    pub trace_id: Option<u64>,
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Latest-frame mailbox between an engine and the widget. Engines
/// overwrite the slot on every paint; the widget reads it on every draw.
pub type FrameSlot = Arc<Mutex<Option<Frame>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Winit-independent so engines don't take a winit dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    /// Usually one grapheme.
    Character(String),
    Named(NamedKey),
}

/// Forwarded into the page so editing shortcuts (Ctrl+Backspace,
/// Shift+Arrow, …) reach the web content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per physical modifier key, mirroring winit's own \
              modifiers state; `apps/browser` constructs this by field name \
              across the crate boundary, so reshaping it into flags is out \
              of scope for a lint fix"
)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

/// An actual session entry; ids are engine-owned and valid for its snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationEntry {
    pub id: u32,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavigationDirection {
    #[default]
    Back,
    Forward,
}

impl NavigationDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }

    pub fn from_wire(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some("back") => Some(Self::Back),
            Some("forward") => Some(Self::Forward),
            Some(_) => None,
        }
    }
}

/// An owned main-frame navigation. Its final URL may differ after redirects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationCommand {
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    HistoryEntry { history_request_id: u32, id: u32 },
}

impl NavigationCommand {
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Navigate { url } => !url.is_empty() && url.len() <= 8192,
            Self::HistoryEntry {
                history_request_id,
                id,
            } => navigation_request_id(*history_request_id) && navigation_request_id(*id),
            Self::Back | Self::Forward | Self::Reload => true,
        }
    }
}

pub(crate) fn navigation_request_id(id: u32) -> bool {
    id > 0 && i32::try_from(id).is_ok()
}

/// Bounded snapshot for one committed top-level origin and document generation.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteStatus {
    pub request_id: u32,
    pub origin: String,
    pub generation: u64,
    pub blocked: u64,
    pub global_enabled: bool,
    pub excepted: bool,
    pub effective: bool,
    pub security: String,
    pub certificate_status: u32,
    pub tls_version: u32,
    pub content_status: u32,
    pub subject: String,
    pub issuer: String,
    /// Chromium `base::Time` microseconds since 1601-01-01.
    pub expires: f64,
    pub permissions: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WebViewCommand {
    /// A DevTools command for extension management: the engine allows only
    /// the `Extensions` domain and what reading an extension's service worker
    /// needs. The reply is a [`WebViewEvent::DevToolsResult`] with the same
    /// `request_id`. `session` addresses an attached service-worker target.
    DevTools {
        request_id: u32,
        method: String,
        params: serde_json::Value,
        session: Option<String>,
    },
    Credentials(CredentialRequest),
    ImportServiceSession(ServiceSessionRequest),
    SetSiteExceptions(Vec<String>),
    RequestSiteStatus {
        request_id: u32,
        origin: String,
    },
    SiteAction {
        request_id: u32,
        origin: String,
        generation: u64,
        action: String,
    },
    /// Load the given (already normalised) URL.
    Navigate(String),
    /// Transport representation only: use `WebViewEngine::navigate_owned`
    /// to observe admission before relying on an owned outcome.
    ScopedNavigation {
        request_id: u32,
        command: NavigationCommand,
    },
    CancelScopedNavigation {
        request_id: u32,
    },
    RequestNavigationHistory {
        request_id: u32,
        direction: NavigationDirection,
    },
    NavigateToHistoryEntry {
        request_id: u32,
        id: u32,
    },
    GoBack,
    GoForward,
    Reload,
    StopLoading,
    /// Ask the native browser host to leave document/video fullscreen.
    ExitFullscreen,
    /// Mute or restore native page audio without installing an audio capture handler.
    SetAudioMuted(bool),
    Find {
        id: u32,
        text: String,
        forward: bool,
        next: bool,
    },
    StopFinding,
    ContextMenuAction {
        id: u32,
        command: u32,
    },
    SetZoomLevel(f64),
    /// Report the document's size and scroll as [`WebViewEvent::DocumentSize`]
    /// whenever they change, or stop. Off until asked: an embedder that sizes
    /// the view to its content asks (a mail reader scrolling as one page).
    /// An engine that cannot measure never reports.
    ReportDocumentSize(bool),
    /// Viewport size in physical pixels plus the display scale factor.
    Resize {
        width: u32,
        height: u32,
        scale_factor: f32,
    },
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: f32,
        y: f32,
    },
    /// Scroll by (dx, dy) physical pixels at pointer position (x, y).
    Scroll {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
    },
    Key {
        key: KeyInput,
        down: bool,
        modifiers: KeyModifiers,
    },
    /// Keyboard focus entered / left the page surface.
    SetFocused(bool),
    /// Compiled `WebKit` content-blocker JSON (`None` clears the filter).
    /// Backends without native content filtering ignore this.
    SetContentFilter(Option<Arc<String>>),
    /// Source filter rules for engines with native matching. An empty source
    /// preserves current rules. `None` preserves the enabled state, so an
    /// asynchronous list refresh cannot undo a user's disable action.
    SetNativeFilterRules {
        rules: Arc<String>,
        enabled: Option<bool>,
    },
    /// Several compiled content-blocker JSON blobs, split because the full
    /// filter-list rule set exceeds `WebKit`'s per-filter cap. An empty
    /// `chunks` clears all filters.
    ///
    /// `version` hashes the *source* filter lists (not the compiled JSON,
    /// since compiler rule ordering isn't stable across runs); the engine
    /// caches each compiled filter under `version`+index so an unchanged
    /// rule set loads from disk instead of recompiling (~280k rules costs
    /// several seconds).
    SetContentFilters {
        version: u64,
        chunks: Arc<Vec<Arc<String>>>,
    },
    /// Scriptlet-style blocking the declarative content filter can't
    /// express (e.g. skipping `YouTube` video ads). Run at document-start
    /// in all frames.
    SetUserScripts(Arc<Vec<String>>),
    /// Native blocker supplements, guarded by the top-level site policy.
    SetBlockingScripts(Arc<Vec<String>>),
    /// Find the most useful visible HTML video in the current page. The
    /// result is delivered asynchronously as [`WebViewEvent::MediaElement`]
    /// or [`WebViewEvent::MediaElementUnavailable`]. Browser chrome uses the
    /// returned viewport geometry to create a live, element-sized `PiP` rather
    /// than mirroring the whole page.
    RequestMediaElement {
        request_id: u64,
    },
    /// Re-resolve a previously selected media element by its stable page-local
    /// identity. Active `PiP` uses this without silently switching videos.
    RequestMediaElementById {
        request_id: u64,
        element_id: String,
    },
    /// Read or operate the current page's content. Exactly one
    /// [`WebViewEvent::PageResult`] with the same id answers it; engines
    /// without page automation answer [`PageFailure::unavailable`].
    Page {
        request_id: u32,
        request: PageRequest,
    },
    /// Capture the page's layout for the paint stream: every box and text
    /// line the engine laid out (see `tilo_paint_stream::snapshot`). Exactly
    /// one [`WebViewEvent::PaintSnapshot`] with the same id answers it.
    PaintCapture {
        request_id: u32,
    },
    /// The bytes of a resource the page already loaded, read from the
    /// engine's cache. One [`WebViewEvent::PaintResource`] answers it.
    PaintResource {
        request_id: u32,
        url: String,
    },
    /// Clear all persistent website data (cookies, caches, storage,
    /// service workers). Backends without a data store ignore this.
    ClearWebsiteData,
    SetClearOnExit(bool),
    ShowDeveloperTools,
    PermissionDecision {
        id: u32,
        allow: bool,
    },
    DownloadDecision {
        id: u32,
        allow: bool,
    },
    CancelNativeDownload {
        id: u32,
    },
    /// Retry a user-selected network GET without navigating the visible page.
    RetryDownload {
        id: u32,
    },
    /// Stop admitting downloads and cancel every pending or active transfer.
    CancelAllDownloads,
    /// Start a user-requested download of an absolute HTTP(S) URL.
    StartDownload {
        uri: String,
    },
    /// Finish `WebKit`'s asynchronous destination decision with the exact
    /// absolute path granted by the desktop file portal.
    SetDownloadDestination {
        id: String,
        path: String,
    },
    /// Cancel an awaiting or active download by its engine-issued stable id.
    CancelDownload {
        id: String,
    },
}

/// Navigation / UI state changes from the engine to the host app, folded
/// into its own state (address bar text, title, progress) during `sync`.
#[derive(Debug, Clone, PartialEq)]
pub enum WebViewEvent {
    /// The reply to a [`WebViewCommand::DevTools`]: the JSON `result` when
    /// `ok`, otherwise the JSON `error` (or a message).
    DevToolsResult {
        request_id: u32,
        ok: bool,
        body: String,
    },
    CredentialResult(CredentialReply),
    ServiceSessionResult(ServiceSessionReply),
    SiteStatus(SiteStatus),
    SiteResult {
        request_id: u32,
        origin: String,
        generation: u64,
        action: String,
        success: bool,
        message: String,
    },
    TabClosed,
    /// The visible route is retired while CEF remains alive solely as a
    /// bounded transfer owner. Physical destruction still emits `TabClosed`.
    TabDetached,
    TabCloseBlocked,
    ContextMenu {
        id: u32,
        x: f64,
        y: f64,
        link: String,
        items: Vec<(u32, String)>,
    },
    ContextMenuClosed {
        id: u32,
    },
    FindResult {
        id: u32,
        count: i32,
        active: i32,
        final_update: bool,
    },
    ZoomChanged(f64),
    NavigationHistory {
        request_id: u32,
        direction: NavigationDirection,
        entries: Vec<NavigationEntry>,
    },
    PermissionRequested {
        id: u32,
        origin: String,
        permissions: String,
    },
    PermissionClosed {
        id: u32,
    },
    NativeDownloadRequested {
        id: u32,
        url: String,
        name: String,
    },
    DownloadUpdated {
        id: u32,
        state: String,
        path: String,
        name: String,
        url: String,
        retryable: bool,
        received: u64,
        total: u64,
        error: String,
    },
    /// Runtime-wide native ownership evidence used by explicit window exit.
    DownloadRuntime {
        admitted: u32,
        detached: u32,
        accepting: bool,
        error: String,
    },
    UriChanged(String),
    TitleChanged(String),
    FaviconChanged {
        page_url: String,
        width: u32,
        height: u32,
        png: Arc<Vec<u8>>,
    },
    AudioMutedChanged(bool),
    MediaPlayingChanged {
        page_url: String,
        document_generation: u64,
        playing: bool,
    },
    /// The page's CEF content-fullscreen intent changed.
    FullscreenChanged(bool),
    /// The renderer owning this tab exited without a reliable fullscreen edge.
    RendererTerminated,
    /// Load progress in `0.0..=1.0`.
    LoadProgress(f64),
    LoadFinished,
    /// A terminal outcome for an admitted request, independent of visual load events.
    NavigationOutcome {
        request_id: u32,
        uri: String,
        error: Option<String>,
    },
    /// Completion tagged with its document URL, so stale startup loads can be ignored.
    LoadFinishedAt {
        uri: String,
    },
    LoadFailed {
        uri: String,
        error: String,
    },
    HistoryChanged {
        can_go_back: bool,
        can_go_forward: bool,
    },
    MediaElement {
        request_id: u64,
        element: MediaElementInfo,
    },
    MediaElementUnavailable {
        request_id: u64,
    },
    PageResult {
        request_id: u32,
        result: Result<PageReply, PageFailure>,
    },
    /// The page's document in CSS pixels: its laid-out size, never less than
    /// the view's, and how far it is scrolled down. Sent while
    /// [`WebViewCommand::ReportDocumentSize`] asks for it, when any changes.
    DocumentSize {
        width: f64,
        height: f64,
        scroll_y: f64,
    },
    /// The raw layout snapshot (JSON), or `None` when the engine could not
    /// take one.
    PaintSnapshot {
        request_id: u32,
        snapshot: Option<String>,
    },
    PaintResource {
        request_id: u32,
        bytes: Option<Vec<u8>>,
    },
    /// `WebKit` received response metadata and is waiting for the host's file
    /// portal to grant an explicit destination.
    DownloadRequested {
        id: String,
        uri: String,
        suggested_filename: String,
    },
    DownloadProgress {
        id: String,
        received_bytes: u64,
        total_bytes: Option<u64>,
        progress: f64,
    },
    DownloadFinished {
        id: String,
        path: String,
    },
    DownloadFailed {
        id: String,
        error: String,
        cancelled: bool,
    },
}

/// A visible media element expressed relative to the web viewport. Keeping
/// this normalised makes it independent of physical display scale and lets
/// browser chrome map it onto the current app surface after a resize.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaElementInfo {
    /// Opaque identity stable for the lifetime of the current document.
    pub element_id: String,
    /// `[x, y, width, height]`, each relative to the viewport dimensions.
    pub viewport_rect: [f64; 4],
    pub intrinsic_size: [u32; 2],
    pub label: String,
}

/// A running web engine instance. Implementations are `Send + Sync` so
/// the widget, the host app, and the engine's own thread can share one
/// handle.
pub trait WebViewEngine: Send + Sync {
    fn supports_text_input(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.supports_text_input())
    }

    /// Whether original-field acknowledged Undo/Redo is supported.
    fn supports_guarded_edit_history(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.supports_guarded_edit_history())
    }

    /// Offer system Undo/Redo to this page's native editing history.
    /// True means the backend consumed the request, not that a history entry
    /// existed or that an asynchronous edit has already completed.
    fn handle_undo_request(&self, request: crate::undo::UndoRequest) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.handle_undo_request(request))
    }

    /// Private text transactions never enter the app event or page-agent stream.
    fn text_input(&self, request: text_input::Request) -> Option<text_input::Receipt> {
        self.wrapped()?.text_input(request)
    }

    /// Create an independent document in the same runtime/profile.
    ///
    /// # Errors
    ///
    /// Returns an error when tabs are unsupported, capacity is exhausted, or the
    /// engine cannot create another document.
    fn create_tab(&self) -> Result<SharedEngine, String> {
        Err("This engine does not support tabs".into())
    }
    /// Suspend painting without discarding the document or its session history.
    fn set_visible(&self, _visible: bool) {}
    /// Keep pointer modifiers current even when no key event reaches the page.
    fn set_modifiers(&self, modifiers: KeyModifiers) {
        if let Some(engine) = self.wrapped() {
            engine.set_modifiers(modifiers);
        }
    }
    /// Native new-window intents, delivered to browser chrome as tab requests.
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        Vec::new()
    }

    /// Whether this backend accepts source rules instead of `WebKit` JSON.
    fn supports_native_filtering(&self) -> bool {
        false
    }

    /// Whether [`WebViewCommand::ReportDocumentSize`] is answered: the
    /// engine measures its document. Known once the engine is ready.
    fn measures_document(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.measures_document())
    }

    /// Enqueue a command. Must never block on engine work.
    fn send(&self, cmd: WebViewCommand);

    /// Admit a navigation and reserve its terminal outcome. IDs must increase
    /// strictly within this engine's lifetime and fit a positive CEF signed integer.
    ///
    /// # Errors
    /// Returns an error without admission if the backend, request or capacity
    /// cannot support the operation. A successful admission completes only with
    /// the matching `NavigationOutcome`, or is invalidated by cancellation.
    fn navigate_owned(&self, request_id: u32, command: NavigationCommand) -> Result<(), String> {
        if !navigation_request_id(request_id) || !command.valid() {
            return Err("Invalid owned navigation request".into());
        }
        if let Some(engine) = self.wrapped() {
            return engine.navigate_owned(request_id, command);
        }
        Err("This engine does not support owned navigation".into())
    }

    fn frames(&self) -> FrameSlot;

    /// Take all pending events, oldest first.
    fn drain_events(&self) -> Vec<WebViewEvent>;

    /// Request native close admission. True allows immediate local close; false
    /// completes asynchronously through `TabClosed`, `TabDetached` or `TabCloseBlocked`.
    fn request_close(&self) -> bool {
        true
    }

    /// Runtime-owned download events, independent of a tab lifetime.
    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        Vec::new()
    }

    /// Invoked (from any thread) whenever a new frame or event is ready —
    /// typically wired to the winit event loop proxy so idle apps repaint
    /// on engine activity. Synchronous engines don't need one.
    fn set_waker(&self, _waker: Box<dyn Fn() + Send + Sync>) {}

    /// A view drew this engine's page just now. Engines that stop painting
    /// pages nobody draws (see [`paint::PaintEngine`]) resume here.
    fn presented(&self) {}

    /// The engine this one wraps, for what only a backend can answer.
    fn wrapped(&self) -> Option<&SharedEngine> {
        None
    }

    /// The page as a display list. An engine that has one is drawn from it
    /// instead of [`frames`](Self::frames) (see [`paint`]).
    fn paint_frame(&self) -> Option<Arc<tilo_paint_stream::Frame>> {
        None
    }

    /// Encoded bytes of an image a [`paint_frame`](Self::paint_frame) references.
    fn paint_blob(&self, _hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        None
    }

    /// The page as a stream for a session to share: a whole frame when
    /// `keyframe` (or when the consumer must start over), else the patches
    /// since the last call, followed by images not sent yet. Empty for an
    /// engine without a display list.
    fn take_paint_stream(&self, _keyframe: bool) -> Vec<tilo_paint_stream::wire::Payload> {
        Vec::new()
    }
}

/// Shared engine handle, as stored by the widget and the host app.
pub type SharedEngine = Arc<dyn WebViewEngine>;
