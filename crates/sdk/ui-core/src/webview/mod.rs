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
pub use page::{PageFailure, PageKey, PageNavigation, PageReply, PageRequest};
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

impl KeyModifiers {
    /// The platform's command modifier alone (Command on macOS, Control
    /// elsewhere), for a host that sends a page its own shortcut: select
    /// all, copy.
    #[must_use]
    pub fn command(conventions: crate::platform::PlatformConventions) -> Self {
        let command = conventions.command_modifier();
        Self {
            ctrl: command.control_key(),
            meta: command.super_key(),
            ..Self::default()
        }
    }
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
    /// The engine can put the site's permissions back to asking.
    pub can_reset: bool,
}

/// A popup window a page opened, with the size it asked for (0 when it
/// named none), in CSS pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    pub id: u32,
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// API permissions and site patterns, as `chrome.permissions` names a set
/// of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionPermissionSet {
    pub permissions: Vec<String>,
    pub origins: Vec<String>,
}

impl ExtensionPermissionSet {
    pub fn is_empty(&self) -> bool {
        self.permissions.is_empty() && self.origins.is_empty()
    }
}
/// What a page asks with a dialog of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDialogKind {
    /// `alert`: a message to acknowledge.
    Alert,
    /// `confirm`: OK or Cancel.
    Confirm,
    /// `prompt`: a line of text, or Cancel.
    Prompt,
    /// "Leave this page?" from a `beforeunload` handler, or "Reload this
    /// page?" when the person reloads it.
    Leave { reload: bool },
}

/// A question the page waits on until it is answered with
/// [`WebViewCommand::AnswerPageDialog`], or the engine says it no longer
/// waits ([`WebViewEvent::PageDialogClosed`]). Everything but `id` and
/// `kind` is the page's own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDialog {
    pub id: u32,
    pub kind: PageDialogKind,
    /// The asking page's http(s) origin; empty for any other page.
    pub origin: String,
    pub message: String,
    /// What a prompt suggests as its answer.
    pub default_text: String,
}

/// The longest answer to a page's prompt an engine takes, in bytes.
pub const MAX_PAGE_DIALOG_ANSWER: usize = 64 * 1024;

/// What an extension changed of its toolbar button (`chrome.action`). Each
/// field is `None` when it did not change; `Some(None)` takes back what it
/// set for one tab, so that tab shows what every tab shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionActionChange {
    pub badge: Option<Option<String>>,
    pub title: Option<Option<String>>,
    /// The popup's address as Chromium resolved it; empty for none, when a
    /// click goes to the extension instead.
    pub popup: Option<Option<String>>,
    pub enabled: Option<Option<bool>>,
    pub icon: Option<Option<ExtensionActionIcon>>,
}

/// An icon an extension gave its toolbar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionActionIcon {
    /// A file among its own, from the extension's root (`/icons/on.png`).
    Path(String),
    /// Pixels it drew: RGBA rows, `width * height * 4` bytes.
    Pixels {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// A row of a page's context menu, as the engine offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuItem {
    /// A command, chosen by `id` ([`WebViewCommand::ContextMenuAction`]).
    /// `checked` is `Some` for a checkbox or radio item.
    Command {
        id: u32,
        label: String,
        checked: Option<bool>,
        enabled: bool,
    },
    /// A row that opens a nested menu of `items`.
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<ContextMenuItem>,
    },
    Separator,
}

/// A row of an extension's toolbar button menu (`chrome.contextMenus` items
/// made for its button), chosen by `key`
/// ([`WebViewCommand::ExtensionMenuChoice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionMenuItem {
    Command {
        key: String,
        label: String,
        /// `Some` for a checkbox or radio item.
        checked: Option<bool>,
        enabled: bool,
    },
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<ExtensionMenuItem>,
    },
    Separator,
}

/// What an extension's notification (`chrome.notifications`) shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionNotice {
    pub title: String,
    pub message: String,
    /// A line under the message (`contextMessage`), empty for none.
    pub context: String,
    /// A list notification's items: each a title and a message.
    pub items: Vec<(String, String)>,
    /// Its buttons' names, at most two.
    pub buttons: Vec<String>,
    /// How far along a progress notification is, from 0 to 100.
    pub progress: Option<u8>,
}

/// What the person did in the Browser's address after an extension's
/// keyword (the manifest's `omnibox`), which the extension hears of as in
/// Chrome (`chrome.omnibox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniboxInput {
    /// They typed the keyword and a space: what follows is the extension's.
    Started,
    /// What follows the keyword is now `text`; the extension's suggestions
    /// for it answer `request`.
    Changed { text: String, request: u32 },
    /// They pressed Enter on `text`, or chose a suggestion whose content it
    /// is: the extension acts, often sending the page somewhere.
    Entered { text: String },
    /// They left the keyword without Enter.
    Cancelled,
}

/// One suggestion an extension made for what follows its keyword: what
/// choosing it enters, and what it says, in Chrome's markup (`<match>`,
/// `<dim>`, `<url>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmniboxSuggestion {
    pub content: String,
    pub description: String,
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
    /// Answer the page's question `id`: OK (or Leave) when `accept`, else
    /// Cancel (or Stay), with a prompt's `text`.
    AnswerPageDialog {
        id: u32,
        accept: bool,
        text: Option<String>,
    },
    SetZoomLevel(f64),
    /// Report the document's size and scroll as [`WebViewEvent::DocumentSize`]
    /// whenever they change, or stop. Off until asked: an embedder that sizes
    /// the view to its content asks (a mail reader scrolling as one page).
    /// An engine that cannot measure never reports.
    ReportDocumentSize(bool),
    /// Read the page's selection as text, answered by one
    /// [`WebViewEvent::SelectionText`] with the same id, for the embedder to
    /// put on the system clipboard. An engine that cannot read it never
    /// answers.
    CopySelection {
        request_id: u32,
    },
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
    /// The person set a permission question aside (Escape, a click
    /// elsewhere) without answering: the site may ask again, as when a
    /// prompt in Chrome is dismissed. An engine that cannot tell this from
    /// no takes it as no.
    PermissionDismissed {
        id: u32,
    },
    /// What an installed extension may do with the Browser's tabs: its API
    /// permissions and the sites it may read and script. The engine answers
    /// the extension's tab APIs within them.
    ExtensionAccess {
        id: String,
        permissions: Vec<String>,
        /// Its host permissions: the sites the tab APIs let it read and
        /// script.
        hosts: Vec<String>,
        /// Where its declared content scripts run, which, as in Chrome, gives
        /// no access to the tab APIs.
        scriptable: Vec<String>,
        /// What its service worker shows the engine to be known as its.
        token: Option<String>,
        /// Its keyboard shortcuts: the manifest's `commands` object, whose
        /// suggested keys the engine gives and runs.
        commands: Option<serde_json::Value>,
    },
    /// What an extension may ask for later (its manifest's
    /// `optional_permissions` and `optional_host_permissions`), and what of
    /// it the person granted: the engine answers `chrome.permissions` within
    /// them, and its tab APIs within what was granted too.
    ExtensionGrants {
        id: String,
        optional: ExtensionPermissionSet,
        granted: ExtensionPermissionSet,
    },
    /// The person answered an extension's request for more access
    /// ([`WebViewEvent::ExtensionPermissionRequest`]).
    ExtensionPermissionDecision {
        request: u32,
        allow: bool,
    },
    /// An extension that left the profile: it is told nothing more.
    ExtensionForget {
        id: String,
    },
    /// The Browser's own extension, which tells the engine which tab a page
    /// is among the ids extensions know tabs by.
    ExtensionIdentity {
        id: String,
    },
    /// This tab shows an extension's popup or settings page over the page
    /// used last: to extensions it is no tab, and that page is its window.
    /// `action` names the extension whose toolbar button opened it, which
    /// may then script that page, as in Chrome.
    ExtensionPanel {
        action: Option<String>,
    },
    /// This tab shows the page of sign-in `flow` of extension `id`
    /// ([`WebViewEvent::ExtensionSignIn`]), over the page used last or out
    /// of sight: to extensions it is no tab, and its closing turns the
    /// sign-in down.
    ExtensionSignInPanel {
        id: String,
        flow: u32,
    },
    /// The person clicked an extension's toolbar button over this page: the
    /// extension hears of it (`action.onClicked`) and may script the page.
    ExtensionClick {
        id: String,
    },
    /// The person chose the item `key` of extension `id`'s toolbar button
    /// menu over this page: the extension hears of it
    /// (`contextMenus.onClicked`).
    ExtensionMenuChoice {
        id: String,
        key: String,
    },
    /// The person chose the notification `notification` of the extension
    /// `id`: its body, or the button at `button`. The extension hears of it
    /// and of the notification closing.
    ExtensionNotificationChosen {
        id: String,
        notification: String,
        button: Option<u8>,
    },
    /// The person typed after the extension's keyword in this page's
    /// address: the extension hears of it (`chrome.omnibox`), and Enter lets
    /// it act on the page as a click on its button would.
    ExtensionOmnibox {
        id: String,
        input: OmniboxInput,
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
    /// An extension changed its toolbar button, for this tab alone or for
    /// every tab (`global`).
    ExtensionAction {
        id: String,
        global: bool,
        change: ExtensionActionChange,
    },
    /// A new document in this tab: what extensions set for their buttons on
    /// this tab alone is gone.
    ExtensionActionReset,
    /// An extension asked to open its popup (`action.openPopup`) over this
    /// tab's page: `url` is among its own pages.
    ExtensionOpenPopup {
        id: String,
        url: String,
    },
    /// The person pressed the keyboard shortcut of an extension's toolbar
    /// button (`_execute_action`) over this tab's page: act as a click on
    /// the button does.
    ExtensionExecuteAction {
        id: String,
    },
    /// What extension `id`'s toolbar button menu holds now; empty when it
    /// holds nothing.
    ExtensionMenu {
        id: String,
        items: Vec<ExtensionMenuItem>,
    },
    /// An extension showed or changed its notification `notification`
    /// (`notice`), or took it away (`None`). A notification is no tab's:
    /// it comes through the page the person used last.
    ExtensionNotification {
        id: String,
        notification: String,
        notice: Option<ExtensionNotice>,
    },
    /// An extension asks for more access (`chrome.permissions.request`),
    /// all of it among what its manifest offers for later: ask the person,
    /// and answer with [`WebViewCommand::ExtensionPermissionDecision`].
    ExtensionPermissionRequest {
        request: u32,
        id: String,
        asked: ExtensionPermissionSet,
    },
    /// What an extension holds of what it may ask for changed: it was
    /// granted more, or gave some up. `granted` is all it holds now, which
    /// the Browser keeps with the extension.
    ExtensionPermissions {
        id: String,
        granted: ExtensionPermissionSet,
    },
    /// An extension asked to open one of the engine's complete downloads
    /// (`chrome.downloads.open`), or show it in its folder (`reveal`,
    /// `chrome.downloads.show`), from this tab's window.
    ExtensionDownloadOpen {
        id: u32,
        reveal: bool,
    },
    /// An extension asks the person to sign in on the web page `url`
    /// (`identity.launchWebAuthFlow`), over this tab's page: show it where
    /// its popup shows, or when it may not ask the person (`interactive`
    /// false) load it out of sight, in a tab marked with
    /// [`WebViewCommand::ExtensionSignInPanel`] for sign-in `flow`. The
    /// engine ends the sign-in, and closes that tab, once the page goes to
    /// the extension's own address.
    ExtensionSignIn {
        id: String,
        url: String,
        interactive: bool,
        flow: u32,
    },
    /// What an extension suggests for what follows its keyword in this
    /// page's address, answering `request` ([`OmniboxInput::Changed`]).
    ExtensionOmniboxSuggestions {
        id: String,
        request: u32,
        suggestions: Vec<OmniboxSuggestion>,
    },
    /// The suggestion an extension shows first while its keyword is typed
    /// (`omnibox.setDefaultSuggestion`), in Chrome's markup with `%s` for
    /// what is typed; `None` for none.
    ExtensionOmniboxDefault {
        id: String,
        description: Option<String>,
    },
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
        items: Vec<ContextMenuItem>,
    },
    ContextMenuClosed {
        id: u32,
    },
    /// The page asks a question of its own and waits for its answer.
    PageDialog(PageDialog),
    /// The page no longer waits on its question `id`: it was answered, or
    /// the page navigated or asked another.
    PageDialogClosed {
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
    /// The page's selection as text, answering
    /// [`WebViewCommand::CopySelection`]; empty when nothing is selected.
    SelectionText {
        request_id: u32,
        text: String,
    },
    /// The address of the link under the pointer, or empty when it left
    /// the link. Sent by an engine showing sealed documents.
    LinkHovered(String),
    /// Where the active match of find `id` is in the view, in logical
    /// pixels from its top left, so a host that scrolls the page itself (a
    /// mail sized to its whole document) can bring it into sight.
    FindSelection {
        id: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
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

    /// Have the page's popup windows (`window.open` with a size: a sign-in,
    /// a payment) opened as popups of this page, which can answer it,
    /// rather than as tabs: [`Self::take_popups`] then lists them. An engine
    /// that cannot goes on opening them as tabs.
    fn accept_popups(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_popups();
        }
    }

    /// Have the page's own questions (`alert`, `confirm`, `prompt`, "leave
    /// this page?") sent as [`WebViewEvent::PageDialog`] for the embedder to
    /// show and answer. Until then, and in an engine that cannot, each is
    /// declined at once and a page may always be left.
    fn accept_page_dialogs(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_page_dialogs();
        }
    }

    /// The popups the page opened since the last call.
    fn take_popups(&self) -> Vec<Popup> {
        self.wrapped()
            .map(|engine| engine.take_popups())
            .unwrap_or_default()
    }

    /// The engine of a popup [`Self::take_popups`] listed, to show it.
    /// Dropping it closes the popup.
    ///
    /// # Errors
    /// Returns an error when the popup is no longer open or this engine has
    /// none.
    fn adopt_popup(&self, popup: u32) -> Result<SharedEngine, String> {
        match self.wrapped() {
            Some(engine) => engine.adopt_popup(popup),
            None => Err("This engine opens no popups".into()),
        }
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

    /// The pointer shape the page last asked for where the pointer is (a
    /// hand on a link, an I-beam on text); `None` for the default arrow, or
    /// an engine that does not say.
    fn cursor(&self) -> Option<winit::window::CursorIcon> {
        self.wrapped().and_then(|engine| engine.cursor())
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

#[cfg(test)]
mod key_modifier_tests {
    use super::KeyModifiers;
    use crate::platform::{Platform, PlatformConventions};

    /// A page's own shortcut goes with the platform's command modifier:
    /// Command on macOS, Control everywhere else, never both.
    #[test]
    fn the_command_modifier_is_the_platforms() {
        let mac = KeyModifiers::command(PlatformConventions::new(Platform::MacOs));
        assert!(mac.meta && !mac.ctrl && !mac.shift && !mac.alt);
        for platform in [Platform::Linux, Platform::Windows, Platform::Android] {
            let other = KeyModifiers::command(PlatformConventions::new(platform));
            assert!(other.ctrl && !other.meta && !other.shift && !other.alt);
        }
    }
}
