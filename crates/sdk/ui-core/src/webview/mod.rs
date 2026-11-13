//! Engine-agnostic embedded web view plumbing. The [`crate::widget::WebView`] widget
//! displays frames produced by a [`WebViewEngine`] backend and forwards input back into
//! it. The engine runs on its own thread (or entirely synchronously, for tests): commands
//! flow in through [`WebViewEngine::send`], rendered pixels come out through a shared
//! [`FrameSlot`], and navigation/UI state changes come out as [`WebViewEvent`]s drained
//! by the host app during `sync`.

pub mod chromium;
mod credentials;
pub mod key_code;
mod service_sessions;
pub use credentials::{
    AddressFields, CredentialAccount, CredentialAction, CredentialForm, CredentialReply,
    CredentialRequest,
};
pub use service_sessions::{ServiceSessionReply, ServiceSessionRequest};
pub mod file_chooser;
pub mod hold;
pub mod http_auth;
pub mod page;
pub mod paint;
pub mod text_input;
pub use file_chooser::{FileChooser, FileChooserMode};
pub use hold::{
    AnsweredBy, HeldKind, HeldRequest, MAX_HELD_CAPTURE, MAX_HELD_METHOD, MAX_HELD_ORIGIN,
};
pub use http_auth::{HttpAuthChallenge, HttpAuthCredentials};
pub use page::{
    FormSubmit, FrameId, MAX_POINTER_STEPS, PageActor, PageChange, PageFailure, PageFieldChange,
    PageFormChange, PageKey, PageNavigation, PageRecordNote, PageReply, PageRequest, PointerButton,
    PointerPoint, PointerStep, PointerStepKind, ScrollTarget,
};
pub mod profile;
pub mod test_engine;

/// The Browser's app id: the app the desktop opens a web address in when nothing else is
/// chosen for it.
pub const BROWSER_APP_ID: &str = "so.tilo.browser";

/// The switch a Browser window is launched with when it is opened for an agent
/// ([`runtime_portal::windows::VISITOR_WINDOW_FLAG`]).
#[cfg(feature = "local-daemons")]
pub const VISITOR_WINDOW_FLAG: &str = runtime_portal::windows::VISITOR_WINDOW_FLAG;

#[cfg(feature = "webview-adblock")]
pub mod adblock;

pub(crate) mod frame_trace;
mod scroll_marker;

use std::sync::{Arc, Mutex};

/// One rendered frame from the engine: tightly-packed RGBA8 at physical pixel resolution.
/// The buffer is shared, never copied per draw — the widget detects staleness via the
/// allocation pointer.
#[derive(Clone)]
pub struct Frame {
    /// Opt-in Rust receipt identity; never implies input causality.
    pub trace_id: Option<u64>,
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub damage: Option<FrameDamage>,
}

/// A rect of a frame, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where a frame differs from the ones its engine made before it: a view that drew one of
/// those repaints, and uploads, only there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameDamage {
    pub generation: u64,
    pub recent: Arc<[(u64, Arc<[PixelRect]>)]>,
}

impl FrameDamage {
    /// What changed since generation `drawn`: nothing when it is this frame's, `None`
    /// when not known.
    #[must_use]
    pub fn since(&self, drawn: u64) -> Option<Vec<PixelRect>> {
        changed_since(self.recent.iter(), self.generation, drawn)
    }
}

/// What changed from generation `from` to `to`, given what each generation changed from
/// the one before it (`recent`, newest first, from `to`).
pub(crate) fn changed_since<'a>(
    recent: impl IntoIterator<Item = &'a (u64, Arc<[PixelRect]>)>,
    to: u64,
    from: u64,
) -> Option<Vec<PixelRect>> {
    let mut rects = Vec::new();
    let mut want = to;
    let mut recent = recent.into_iter();
    while want > from {
        let (generation, changed) = recent.next()?;
        if *generation != want {
            return None;
        }
        rects.extend_from_slice(changed);
        want -= 1;
    }
    (want == from).then_some(rects)
}

/// The page's focused editable field, as an engine reports it for the platform input
/// method: what it takes, and where on the page it and the text being composed in it are,
/// so the input method's candidates appear where the person types.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextField {
    pub purpose: crate::ime::InputPurpose,
    /// Line breaks are text (a text area, an editable document).
    pub multiline: bool,
    /// The field's box, in physical pixels from the view's top-left corner; `None` when
    /// the engine cannot place it (a field in a frame of another site).
    pub bounds: Option<vello::kurbo::Rect>,
    /// The box of the text an input method is composing, in the same pixels, while it
    /// composes.
    pub composition: Option<vello::kurbo::Rect>,
}

/// Latest-frame mailbox between an engine and the widget.
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
    Character(String),
    Named(NamedKey),
}

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
    /// The platform's command modifier alone (Command on macOS, Control elsewhere), for a
    /// host that sends a page its own shortcut: select all, copy.
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

/// A key one of extensions' commands holds (`commands` in a manifest), as Chrome names it
/// ("Y", "9", "Comma", "Up"), with its modifiers: `meta` is Command on macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionShortcut {
    pub key: String,
    pub modifiers: KeyModifiers,
}

impl ExtensionShortcut {
    /// The shortcut a key pressed makes, its key named as Chrome names it, as the engine
    /// host reads the keys the Browser forwards (`extension_commands.h`, `KeyShortcut`):
    /// with Shift a digit, a comma or a period is the US layout's symbol on it.
    #[must_use]
    pub fn pressed(key: &KeyInput, modifiers: KeyModifiers) -> Option<Self> {
        let key = match key {
            KeyInput::Named(named) => match named {
                NamedKey::ArrowUp => "Up",
                NamedKey::ArrowDown => "Down",
                NamedKey::ArrowLeft => "Left",
                NamedKey::ArrowRight => "Right",
                NamedKey::Home => "Home",
                NamedKey::End => "End",
                NamedKey::PageUp => "PageUp",
                NamedKey::PageDown => "PageDown",
                NamedKey::Delete => "Delete",
                NamedKey::Tab => "Tab",
                NamedKey::Enter | NamedKey::Backspace | NamedKey::Escape => return None,
            }
            .to_owned(),
            KeyInput::Character(text) => {
                let mut chars = text.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                match c {
                    c if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
                    ',' | '<' => "Comma".to_owned(),
                    '.' | '>' => "Period".to_owned(),
                    ' ' => "Space".to_owned(),
                    c => {
                        let digit = ")!@#$%^&*(".find(c)?;
                        digit.to_string()
                    }
                }
            }
        };
        Some(Self { key, modifiers })
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

/// An owned main-frame navigation.
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
    pub can_reset: bool,
}

/// A popup window a page opened, with the size it asked for (0 when it named none), in
/// CSS pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    pub id: u32,
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// API permissions and site patterns, as `chrome.permissions` names a set of them.
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
    Alert,
    Confirm,
    Prompt,
    Leave { reload: bool },
}

/// A question the page waits on until it is answered with
/// [`WebViewCommand::AnswerPageDialog`], or the engine says it no longer waits
/// ([`WebViewEvent::PageDialogClosed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDialog {
    pub id: u32,
    pub kind: PageDialogKind,
    pub origin: String,
    pub message: String,
    pub default_text: String,
}

/// The longest answer to a page's prompt an engine takes, in bytes.
pub const MAX_PAGE_DIALOG_ANSWER: usize = 64 * 1024;

/// What an extension changed of its toolbar button (`chrome.action`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionActionChange {
    pub badge: Option<Option<String>>,
    pub title: Option<Option<String>>,
    pub popup: Option<Option<String>>,
    pub enabled: Option<Option<bool>>,
    pub icon: Option<Option<ExtensionActionIcon>>,
}

/// What an extension asks of its side panel beside a tab's page; each address is among
/// the extension's own pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionSidePanelChange {
    Open(String),
    Toggle(String),
    Options(Option<String>),
    Close,
}

/// An icon an extension gave its toolbar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionActionIcon {
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
    Command {
        id: u32,
        label: String,
        checked: Option<bool>,
        enabled: bool,
    },
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<ContextMenuItem>,
    },
    Separator,
}

/// A row of an extension's toolbar button menu (`chrome.contextMenus` items made for its
/// button), chosen by `key` ([`WebViewCommand::ExtensionMenuChoice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionMenuItem {
    Command {
        key: String,
        label: String,
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
    pub context: String,
    pub items: Vec<(String, String)>,
    pub buttons: Vec<String>,
    /// How far along a progress notification is, from 0 to 100.
    pub progress: Option<u8>,
    pub silent: bool,
}

/// What the person did in the Browser's address after an extension's keyword (the
/// manifest's `omnibox`), which the extension hears of as in Chrome (`chrome.omnibox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniboxInput {
    Started,
    Changed { text: String, request: u32 },
    Entered { text: String },
    Cancelled,
}

/// One suggestion an extension made for what follows its keyword: what choosing it
/// enters, and what it says, in Chrome's markup (`<match>`, `<dim>`, `<url>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmniboxSuggestion {
    pub content: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WebViewCommand {
    /// A DevTools command for extension management: the engine allows only the
    /// `Extensions` domain and what reading an extension's service worker needs.
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
    Navigate(String),
    /// Transport representation only: use `WebViewEngine::navigate_owned` to observe
    /// admission before relying on an owned outcome.
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
    ExitFullscreen,
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
    AnswerPageDialog {
        id: u32,
        accept: bool,
        text: Option<String>,
    },
    /// Answer the page's file chooser `id` ([`WebViewEvent::FileChooser`])
    /// with exactly the paths the person picked in the desktop's picker, or
    /// with none to cancel it. Answered once: a second answer, or one for a
    /// chooser the page no longer waits on, does nothing.
    AnswerFileChooser {
        id: u32,
        paths: Vec<String>,
    },
    /// Answer the sign-in `id` a server asked for ([`WebViewEvent::HttpAuth`])
    /// with the person's credentials, or with none to cancel it, which shows
    /// the server's own page. `asker` is who the person was shown asking: an
    /// answer naming another asker than the challenge's, or a challenge the
    /// request no longer waits on, sends nothing.
    AnswerHttpAuth {
        id: u32,
        asker: String,
        credentials: Option<HttpAuthCredentials>,
    },
    /// Answer the request `id` an agent's window held ([`WebViewEvent::HeldRequest`]):
    /// let it leave exactly once, or keep the page as it is and drop it.
    /// `by` is who answered: the engine sets it down in the tab's record beside how the hold
    /// ended, since it cannot know.
    AnswerHeldRequest {
        id: u32,
        allow: bool,
        by: AnsweredBy,
    },
    SetZoomLevel(f64),
    /// Report the document's size and scroll as [`WebViewEvent::DocumentSize`] whenever
    /// they change, or stop. An engine that cannot measure never reports.
    ReportDocumentSize(bool),
    /// Read the page's selection as text, answered by one [`WebViewEvent::SelectionText`]
    /// with the same id, for the embedder to put on the system clipboard. An engine that
    /// cannot read it never answers.
    CopySelection {
        request_id: u32,
    },
    AnswerClipboardRead {
        request_id: u32,
        text: Option<String>,
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
        code: Option<&'static str>,
    },
    /// The input method's text being composed at the page's caret, in place of any
    /// composed before.
    ImeComposition {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    ImeCommit(String),
    ImeCancel,
    ImeFinish,
    SetFocused(bool),
    SetContentFilter(Option<Arc<String>>),
    SetNativeFilterRules {
        rules: Arc<String>,
        enabled: Option<bool>,
    },
    /// Several compiled content-blocker JSON blobs, split because the full filter-list
    /// rule set exceeds `WebKit`'s per-filter cap.
    SetContentFilters {
        version: u64,
        chunks: Arc<Vec<Arc<String>>>,
    },
    SetUserScripts(Arc<Vec<String>>),
    SetBlockingScripts(Arc<Vec<String>>),
    RequestMediaElement {
        request_id: u64,
    },
    RequestMediaElementById {
        request_id: u64,
        element_id: String,
    },
    /// `actor` names who asks, when the caller admitted someone by name: several actors may
    /// work in one tab, and the engine keeps what each has read, and what each did, under that
    /// name. `None` is the one unnamed actor a tab has always had.
    Page {
        request_id: u32,
        actor: Option<PageActor>,
        request: PageRequest,
    },
    /// Stop the page action `request_id` names. An action nobody waits for any more must not
    /// hold the page: the tab answers it as settled with an unknown outcome rather than
    /// running the next action behind it. The request is named because several actors'
    /// actions may be in one tab: a caller that gives up stops its own action and never
    /// another's.
    CancelPage {
        request_id: u32,
    },
    PaintCapture {
        request_id: u32,
    },
    /// The bytes of a resource the page already loaded, read from the engine's cache.
    PaintResource {
        request_id: u32,
        url: String,
    },
    ClearWebsiteData,
    SetClearOnExit(bool),
    ShowDeveloperTools,
    PermissionDecision {
        id: u32,
        allow: bool,
    },
    PermissionDismissed {
        id: u32,
    },
    ExtensionAccess {
        id: String,
        permissions: Vec<String>,
        hosts: Vec<String>,
        scriptable: Vec<String>,
        framed: Vec<String>,
        token: Option<String>,
        commands: Option<serde_json::Value>,
    },
    ExtensionGrants {
        id: String,
        optional: ExtensionPermissionSet,
        granted: ExtensionPermissionSet,
    },
    ExtensionPermissionDecision {
        request: u32,
        allow: bool,
    },
    ExtensionForget {
        id: String,
    },
    ExtensionIdentity {
        id: String,
    },
    ExtensionPanel {
        action: Option<String>,
    },
    ExtensionSignInPanel {
        id: String,
        flow: u32,
    },
    ExtensionClick {
        id: String,
    },
    ExtensionMenuChoice {
        id: String,
        key: String,
    },
    ExtensionNotificationChosen {
        id: String,
        notification: String,
        button: Option<u8>,
    },
    ExtensionOmnibox {
        id: String,
        input: OmniboxInput,
    },
    ExtensionOpenSidePanel {
        id: String,
    },
    ExtensionSidePanelPage {
        id: String,
    },
    ExtensionCommandKey {
        key: KeyInput,
        modifiers: KeyModifiers,
    },
    /// Save the download `id` the page offered, or discard it. `by` is who
    /// answered: of a download an agent's window held for the person, the
    /// engine sets it down in the tab's record as it does for
    /// [`Self::AnswerHeldRequest`].
    DownloadDecision {
        id: u32,
        allow: bool,
        by: AnsweredBy,
    },
    CancelNativeDownload {
        id: u32,
    },
    RetryDownload {
        id: u32,
    },
    CancelAllDownloads,
    StartDownload {
        uri: String,
    },
    SetDownloadDestination {
        id: String,
        path: String,
    },
    CancelDownload {
        id: String,
    },
}

/// Navigation / UI state changes from the engine to the host app, folded into its own
/// state (address bar text, title, progress) during `sync`.
#[derive(Debug, Clone, PartialEq)]
pub enum WebViewEvent {
    /// The reply to a [`WebViewCommand::DevTools`]: the JSON `result` when `ok`,
    /// otherwise the JSON `error` (or a message).
    DevToolsResult {
        request_id: u32,
        ok: bool,
        body: String,
    },
    CredentialResult(CredentialReply),
    ServiceSessionResult(ServiceSessionReply),
    ExtensionAction {
        id: String,
        global: bool,
        change: ExtensionActionChange,
    },
    ExtensionActionReset,
    ExtensionOpenPopup {
        id: String,
        url: String,
    },
    ExtensionExecuteAction {
        id: String,
    },
    ExtensionShortcuts(Vec<ExtensionShortcut>),
    ExtensionMenu {
        id: String,
        items: Vec<ExtensionMenuItem>,
    },
    ExtensionNotification {
        id: String,
        notification: String,
        notice: Option<ExtensionNotice>,
    },
    ExtensionPermissionRequest {
        request: u32,
        id: String,
        asked: ExtensionPermissionSet,
    },
    ExtensionPermissions {
        id: String,
        granted: ExtensionPermissionSet,
    },
    ExtensionPermissionClosed {
        request: u32,
    },
    ExtensionDownloadOpen {
        id: u32,
        reveal: bool,
    },
    ExtensionSignIn {
        id: String,
        url: String,
        interactive: bool,
        flow: u32,
    },
    ExtensionOmniboxSuggestions {
        id: String,
        request: u32,
        suggestions: Vec<OmniboxSuggestion>,
    },
    ExtensionOmniboxDefault {
        id: String,
        description: Option<String>,
    },
    ExtensionSidePanel {
        id: String,
        change: ExtensionSidePanelChange,
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
    PageDialog(PageDialog),
    PageDialogClosed {
        id: u32,
    },
    /// A person opened the page's file chooser, and the page waits for the
    /// files ([`WebViewCommand::AnswerFileChooser`]).
    FileChooser(FileChooser),
    /// The page no longer waits on its file chooser `id`: it was answered,
    /// its document left or its tab closed, another chooser took its place,
    /// or nobody answered in the engine's time.
    FileChooserClosed {
        id: u32,
    },
    /// A server (or a proxy) asks for a user name and a password, and the
    /// request waits for them ([`WebViewCommand::AnswerHttpAuth`]).
    HttpAuth(HttpAuthChallenge),
    /// The request no longer waits on the sign-in `id`: it was answered, the
    /// page went elsewhere or its tab closed, another took its place, or
    /// nobody answered in the engine's time.
    HttpAuthClosed {
        id: u32,
    },
    HeldRequest(HeldRequest),
    HeldRequestClosed {
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
    FullscreenChanged(bool),
    RendererTerminated,
    /// Load progress in `0.0..=1.0`.
    LoadProgress(f64),
    LoadFinished,
    NavigationOutcome {
        request_id: u32,
        uri: String,
        error: Option<String>,
    },
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
    /// The engine set down in the tab's record an entry no action's answer
    /// carried: what a person typed straight into the page. `seq` is its
    /// place in the engine's record and `entry` the entry as
    /// [`PageRequest::Ledger`] lists it, and so page content: never trusted
    /// as instructions. Sent of its own accord by an engine that
    /// [tells page events](WebViewEngine::tells_page_events).
    PageRecord {
        seq: u64,
        entry: serde_json::Value,
    },
    /// Something other than an action's own edit changed what a field of
    /// the page holds or what one of its forms asks: the page's own script,
    /// or a person typing. Sent of its own accord by an engine that
    /// [tells page events](WebViewEngine::tells_page_events), so nobody
    /// reads the page again and again to find out.
    PageChanged(PageChange),
    /// The page's document in CSS pixels: its laid-out size, never less than the view's,
    /// and how far it is scrolled down.
    DocumentSize {
        width: f64,
        height: f64,
        scroll_y: f64,
    },
    SelectionText {
        request_id: u32,
        text: String,
    },
    ClipboardWrite {
        text: String,
        html: Option<String>,
        secret: bool,
    },
    ClipboardRead {
        request_id: u32,
    },
    LinkHovered(String),
    /// Where the active match of find `id` is in the view, in logical pixels from its top
    /// left, so a host that scrolls the page itself (a mail sized to its whole document)
    /// can bring it into sight.
    FindSelection {
        id: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    PaintSnapshot {
        request_id: u32,
        snapshot: Option<String>,
    },
    PaintResource {
        request_id: u32,
        bytes: Option<Vec<u8>>,
    },
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

/// A visible media element expressed relative to the web viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaElementInfo {
    pub element_id: String,
    pub viewport_rect: [f64; 4],
    pub intrinsic_size: [u32; 2],
    pub label: String,
}

/// A running web engine instance. Implementations are `Send + Sync` so the widget, the
/// host app, and the engine's own thread can share one handle.
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
    fn handle_undo_request(&self, request: crate::undo::UndoRequest) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.handle_undo_request(request))
    }

    /// Private text transactions never enter the app event or page-agent stream.
    fn text_input(&self, request: text_input::Request) -> Option<text_input::Receipt> {
        self.wrapped()?.text_input(request)
    }

    /// Create an independent document in the same runtime/profile. Returns an error when
    /// tabs are unsupported, capacity is exhausted, or the engine cannot create another
    /// document.
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

    /// Have the page's popup windows (`window.open` with a size: a sign-in, a payment)
    /// opened as popups of this page, which can answer it, rather than as tabs:
    /// [`Self::take_popups`] then lists them. An engine that cannot goes on opening them
    /// as tabs.
    fn accept_popups(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_popups();
        }
    }

    /// Have the page's own questions (`alert`, `confirm`, `prompt`, "leave this page?")
    /// sent as [`WebViewEvent::PageDialog`] for the embedder to show and answer. Until
    /// then, and in an engine that cannot, each is declined at once and a page may always
    /// be left.
    fn accept_page_dialogs(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_page_dialogs();
        }
    }

    /// Have a file chooser a person opens in the page sent as
    /// [`WebViewEvent::FileChooser`], for the embedder to ask the desktop's
    /// picker and answer. Until then, and in an engine that cannot, the
    /// engine does what its platform does: its own panel where it has one,
    /// and otherwise a chooser cancelled at once. An engine whose platform
    /// panel is the proven way keeps it and sends none (see
    /// [`file_chooser`]).
    fn accept_file_choosers(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_file_choosers();
        }
    }

    /// Have a server's sign-in (HTTP authentication) sent as
    /// [`WebViewEvent::HttpAuth`], for the embedder to ask the person and
    /// answer. Until then, and in an engine that cannot, each challenge is
    /// cancelled at once and the page shows the server's own answer (see
    /// [`http_auth`]).
    fn accept_http_auth(&self) {
        if let Some(engine) = self.wrapped() {
            engine.accept_http_auth();
        }
    }

    /// Hold what the page is about to send, as a window an agent operates does: every
    /// form submission, every navigation or request with a method of its own, every
    /// download and every payment waits for the person's answer
    /// ([`WebViewEvent::HeldRequest`], [`WebViewCommand::AnswerHeldRequest`]). A person's
    /// own window never holds, and an engine that cannot hold sends nothing.
    fn hold_departures(&self) {
        if let Some(engine) = self.wrapped() {
            engine.hold_departures();
        }
    }

    /// Have what the page copies sent as [`WebViewEvent::ClipboardWrite`], and its pastes
    /// and reads asked of the embedder ([`WebViewEvent::ClipboardRead`]), where the
    /// engine cannot reach the system clipboard itself. Until then, and in an engine that
    /// can or cannot ask, the page's clipboard is the engine's own.
    fn share_clipboard(&self) {
        if let Some(engine) = self.wrapped() {
            engine.share_clipboard();
        }
    }

    /// The popups the page opened since the last call.
    fn take_popups(&self) -> Vec<Popup> {
        self.wrapped()
            .map(|engine| engine.take_popups())
            .unwrap_or_default()
    }

    /// The engine of a popup [`Self::take_popups`] listed, to show it. Returns an error
    /// when the popup is no longer open or this engine has none.
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

    /// Whether [`WebViewCommand::ReportDocumentSize`] is answered: the engine measures
    /// its document.
    fn measures_document(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.measures_document())
    }

    /// Whether the engine sends [`WebViewEvent::PageRecord`] and
    /// [`WebViewEvent::PageChanged`] of its own accord: what a person typed
    /// is announced, and so is what the page changed of itself. An embedder
    /// reads a page again on a timer only for an engine that does not.
    /// Known once the engine is ready.
    fn tells_page_events(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.tells_page_events())
    }

    /// Enqueue a command. Must never block on engine work.
    fn send(&self, cmd: WebViewCommand);

    /// Admit a navigation and reserve its terminal outcome. IDs must increase strictly
    /// within this engine's lifetime and fit a positive CEF signed integer. Returns an
    /// error without admission if the backend, request or capacity cannot support the
    /// operation.
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

    /// The sequence number of the display list this engine paints
    /// ([`Self::paint_frame`]'s [`Frame::seq`](tilo_paint_stream::model::Frame::seq)):
    /// moves on with every change to what the page draws, so a widget repaints exactly
    /// when the page did.
    fn paint_seq(&self) -> u64 {
        0
    }

    /// Take all pending events, oldest first.
    fn drain_events(&self) -> Vec<WebViewEvent>;

    /// Request native close admission.
    fn request_close(&self) -> bool {
        true
    }

    /// Runtime-owned download events, independent of a tab lifetime.
    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        Vec::new()
    }

    /// Invoked (from any thread) whenever a new frame or event is ready — typically wired
    /// to the winit event loop proxy so idle apps repaint on engine activity.
    fn set_waker(&self, _waker: Box<dyn Fn() + Send + Sync>) {}

    /// A view drew this engine's page just now.
    fn presented(&self) {}

    /// The engine this one wraps, for what only a backend can answer.
    fn wrapped(&self) -> Option<&SharedEngine> {
        None
    }

    /// Whether the engine takes an input method's composition
    /// ([`WebViewCommand::ImeComposition`], [`WebViewCommand::ImeCommit`]) and reports
    /// the page's focused field ([`Self::text_field`]).
    fn supports_composition(&self) -> bool {
        self.wrapped()
            .is_some_and(|engine| engine.supports_composition())
    }

    /// The page's focused editable field, for the platform input method; `None` while
    /// nothing editable in the page has the focus.
    fn text_field(&self) -> Option<TextField> {
        self.wrapped().and_then(|engine| engine.text_field())
    }

    /// The pointer shape the page last asked for where the pointer is (a hand on a link,
    /// an I-beam on text); `None` for the default arrow, or an engine that does not say.
    fn cursor(&self) -> Option<winit::window::CursorIcon> {
        self.wrapped().and_then(|engine| engine.cursor())
    }

    /// The page as a display list.
    fn paint_frame(&self) -> Option<Arc<tilo_paint_stream::Frame>> {
        None
    }

    /// Encoded bytes of an image a [`paint_frame`](Self::paint_frame) references.
    fn paint_blob(&self, _hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        None
    }

    /// The page as a stream for a session to share: a whole frame when `keyframe` (or
    /// when the consumer must start over), else the patches since the last call, followed
    /// by images not sent yet.
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

    /// A page's own shortcut goes with the platform's command modifier: Command on macOS,
    /// Control everywhere else, never both.
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
