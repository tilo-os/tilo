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
//!  * WPE `WebKit` — the Tilo system engine, rendering real pages offscreen.
//!
//! Coordinates in commands are **physical pixels relative to the web
//! view's top-left corner** — the widget does the logical→physical
//! translation so engines never see toolkit layout space.

pub mod chromium;
mod credentials;
pub use credentials::{
    AddressFields, CredentialAccount, CredentialAction, CredentialForm, CredentialReply,
    CredentialRequest,
};
pub mod page;
pub use page::{PageFailure, PageKey, PageReply, PageRequest};
pub mod profile;
pub mod test_engine;

#[cfg(feature = "webview-adblock")]
pub mod adblock;

#[cfg(feature = "webview-wpe")]
pub mod wpe;

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
    Credentials(CredentialRequest),
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
    CredentialResult(CredentialReply),
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

/// One plane of a [`DmabufFrame`]: its own fd + layout. A GPU-tiled RGBA
/// buffer has 2 planes (pixels + compression metadata); a linear one, 1.
#[derive(Debug)]
#[cfg(unix)]
pub struct DmabufPlane {
    pub fd: media::dmabuf::DmabufHandle,
    pub stride: u32,
    pub offset: u32,
}

/// A GPU frame exported as a DMA-BUF handle (the `webview-wpe` EGL export
/// path). Feature-neutral so it can cross the engine trait — only the WPE
/// engine produces one and only the wgpu import consumes it. `fourcc` +
/// `modifier` describe the pixel format and tiling.
#[derive(Debug)]
#[cfg(unix)]
pub struct DmabufFrame {
    pub planes: Vec<DmabufPlane>,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

/// A running web engine instance. Implementations are `Send + Sync` so
/// the widget, the host app, and the engine's own thread can share one
/// handle.
pub trait WebViewEngine: Send + Sync {
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
    fn set_modifiers(&self, _modifiers: KeyModifiers) {}
    /// Native new-window intents, delivered to browser chrome as tab requests.
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        Vec::new()
    }

    /// Whether this backend accepts source rules instead of `WebKit` JSON.
    fn supports_native_filtering(&self) -> bool {
        false
    }

    /// Enqueue a command. Must never block on engine work.
    fn send(&self, cmd: WebViewCommand);

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

    /// The zero-copy path, if a GPU frame is pending. Default: none — SHM
    /// and test engines deliver pixels through [`frames`](Self::frames).
    #[cfg(unix)]
    fn take_dmabuf(&self) -> Option<DmabufFrame> {
        None
    }
}

/// Shared engine handle, as stored by the widget and the host app.
pub type SharedEngine = Arc<dyn WebViewEngine>;

/// Normalise user-typed address-bar input into a loadable URL: pass
/// through explicit schemes, otherwise treat input with spaces or
/// without dots as a search query and everything else as an `https://`
/// host.
pub fn normalise_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
        || trimmed.starts_with("about:")
    {
        return trimmed.to_string();
    }
    let looks_like_host =
        !trimmed.contains(' ') && (trimmed.contains('.') || trimmed.starts_with("localhost"));
    if looks_like_host {
        format!("https://{trimmed}")
    } else {
        format!(
            "https://duckduckgo.com/?q={}",
            form_urlencoded::byte_serialize(trimmed.as_bytes()).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_keeps_schemes_prepends_https_and_searches_queries() {
        // phase: normalise_passes_through_schemes
        {
            assert_eq!(
                normalise_url("https://example.org/a"),
                "https://example.org/a"
            );
            assert_eq!(normalise_url("http://example.org"), "http://example.org");
            assert_eq!(normalise_url("about:blank"), "about:blank");
        }
        // phase: normalise_prepends_https_for_hosts
        {
            assert_eq!(normalise_url("example.org"), "https://example.org");
            assert_eq!(normalise_url("localhost:8080"), "https://localhost:8080");
        }
        // phase: normalise_searches_for_queries
        {
            assert_eq!(
                normalise_url("rust webview"),
                "https://duckduckgo.com/?q=rust+webview"
            );
            assert_eq!(
                normalise_url("what is Tilo?"),
                "https://duckduckgo.com/?q=what+is+Tilo%3F"
            );
        }
    }
}
