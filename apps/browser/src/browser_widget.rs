//! `BrowserWidget` — the quick-ui root widget for the browser app.
//!
//! Chrome around the toolkit's [`ui::widget::WebView`]: a navigation
//! toolbar (back / forward / reload + address input), a thin progress
//! bar while a page loads, and the live page surface. There is no tab
//! strip — tabbing is the compositor's `WindowGroup` frame primitive, so
//! one browser window shows exactly one page.
//!
//! State model:
//!  * Address text is retained process-local state. The engine updates it
//!    unless the user is editing; an explicit Navigate resource operation is
//!    the only point at which it affects browser behaviour.
//!  * Every button emits its canonical typed resource action. Presentation
//!    operations remain local to this window, while durable settings and
//!    bookmarks are applied through the action registry.
//!  * Everything engine-side (history, page pixels, load progress)
//!    stays in the engine; the widget only mirrors what the chrome
//!    displays.

use automerge::{AutoCommit, ObjId, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionArgs, ActionBinding, ActionContract, ActionParam, ActionSpec, ControlInvocation,
    EncodedAction, OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};
use ui::webview::test_engine::TestEngine;
use ui::webview::{MediaElementInfo, SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::toast::{ToastCard, ToastKind, paint_card};
use ui::widget::{
    AppKind, AppWidget, Button, Column, Container, Continuable, EmptyState,
    FloatingSurfaceProvider, IconDescriptor, ProgressBar, Row, ScrollAxis, Scrollable, Spacer,
    Text, TextInput, TextInputKind, WebView, Widget,
};

use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, history_resource_id,
};
use crate::address_overlay::AddressSuggestions;
use crate::downloads::{DownloadStatus, Downloads};
use crate::history::History;
use crate::library::Bookmarks;
use crate::settings::{Settings, SettingsActionBindings, SettingsPane, SettingsPresentation};

const PIP_ID: &str = "browser.media-pip";
const PIP_ELEMENT_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// How many address-bar suggestions to show.
const SUGGESTION_LIMIT: usize = 8;
/// A library query is local presentation state, but it can also be set by a
/// resource action. Keep the agent-facing operation bounded just like the
/// persisted collections it searches.
const MAX_LIBRARY_FILTER_BYTES: usize = 256;

/// e2e id of the address input (also used internally to detect whether
/// the user is editing it).
const URL_INPUT_ID: &str = "browser.url_input";
const LIBRARY_FILTER_INPUT_ID: &str = "browser.library.filter";
const PIP_FEEDBACK_LIFETIME: std::time::Duration = std::time::Duration::from_millis(4200);
const PIP_FEEDBACK_FADE: std::time::Duration = std::time::Duration::from_millis(180);
const SEARCH_QUERY_TOKEN: &str = "{query}";
/// A `WebView` backend is external work: it can fail to produce either a load
/// completion or an explicit failure.  Never leave an accepted resource
/// invocation pending forever in that case.
const NAVIGATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// Media discovery is asynchronous.  Keep a resource invocation pending only
/// until the engine gives its positive/negative answer, never indefinitely.
const PIP_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// One shared desktop measure keeps the Library title, section switcher, and
/// rows visually related instead of leaving the actions in a disconnected
/// expanse of empty canvas. Compact chrome deliberately remains fluid.
const LIBRARY_DESKTOP_CONTENT_WIDTH: f32 = 920.0;

/// Browser's public navigation boundary. Keep this separate from the generic
/// embedded-view helper: Browser's manifest advertises only these schemes.
fn is_supported_web_url(value: &str) -> bool {
    url::Url::parse(value.trim())
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

/// Host-like text that users conventionally enter without a scheme. Bracketed
/// IPv6 is deliberately included; unbracketed IPv6 is ambiguous with a URL
/// scheme/port and is not a browser address syntax.
fn looks_like_bare_web_host(value: &str) -> bool {
    !value.chars().any(char::is_whitespace)
        && (value.starts_with("localhost")
            || value.contains('.')
            || (value.starts_with('[') && value.contains(']')))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipFeedbackKind {
    Finding,
    NoVideo,
}

struct PipFeedback {
    kind: PipFeedbackKind,
    started: std::time::Instant,
    /// Screenshot/debug guidance is deliberately steady: headless rendering
    /// can be queued behind a busy renderer, and must not race the ordinary
    /// short-lived live toast away before it is inspected.
    persistent: bool,
}

impl PipFeedback {
    fn transient(kind: PipFeedbackKind) -> Self {
        Self {
            kind,
            started: std::time::Instant::now(),
            persistent: false,
        }
    }

    fn persistent(kind: PipFeedbackKind) -> Self {
        Self {
            kind,
            started: std::time::Instant::now()
                .checked_sub(PIP_FEEDBACK_FADE)
                .unwrap_or_else(std::time::Instant::now),
            persistent: true,
        }
    }

    fn is_expired(&self) -> bool {
        !self.persistent && self.started.elapsed() >= PIP_FEEDBACK_LIFETIME
    }
}

const BROWSER_OPERATION_TYPE: &str = "browser.operation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibrarySection {
    History,
    Bookmarks,
    Downloads,
    Extensions,
}

impl LibrarySection {
    fn label(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Bookmarks => "bookmarks",
            Self::Downloads => "downloads",
            Self::Extensions => "extensions",
        }
    }
}

/// Case-insensitive, space-tolerant matching for the small, local Library
/// collections.  Splitting the query gives people a useful "rust docs"
/// experience without growing a second fuzzy-search engine beside the global
/// resource index.
fn library_filter_matches(query: &str, fields: &[&str]) -> bool {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    terms.is_empty()
        || terms.iter().all(|term| {
            fields
                .iter()
                .any(|field| field.to_lowercase().contains(term))
        })
}

fn bounded_library_filter(query: &str) -> String {
    let query = query.trim();
    if query.len() <= MAX_LIBRARY_FILTER_BYTES {
        return query.to_string();
    }
    let mut end = MAX_LIBRARY_FILTER_BYTES;
    while !query.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    query.get(..end).unwrap_or_default().to_string()
}

#[derive(Clone, Debug)]
enum BrowserOperation {
    Back,
    Forward,
    Reload,
    Navigate { url: String },
    ShowLibrary { section: LibrarySection },
    SetLibraryFilter { query: String },
    ClearLibraryFilter,
    ShowSettings,
    ShowPage,
    ClearHistory,
    RequestWebsiteDataClear,
    TogglePictureInPicture,
    ToggleHistoryPause,
    StartDownload { url: String },
    CancelDownload { id: String },
    RetryDownload { id: String },
    OpenDownload { id: String },
    RevealDownload { id: String },
}

fn browser_operation_payload(operation: &BrowserOperation) -> serde_json::Value {
    match operation {
        BrowserOperation::Back => serde_json::json!({ "operation": "back" }),
        BrowserOperation::Forward => serde_json::json!({ "operation": "forward" }),
        BrowserOperation::Reload => serde_json::json!({ "operation": "reload" }),
        BrowserOperation::Navigate { url } => {
            serde_json::json!({ "operation": "navigate", "url": url })
        }
        BrowserOperation::ShowLibrary { section } => serde_json::json!({
            "operation": "show-library",
            "section": match section {
                LibrarySection::History => "history",
                LibrarySection::Bookmarks => "bookmarks",
                LibrarySection::Downloads => "downloads",
                LibrarySection::Extensions => "extensions",
            }
        }),
        BrowserOperation::SetLibraryFilter { query } => {
            serde_json::json!({ "operation": "set-library-filter", "query": query })
        }
        BrowserOperation::ClearLibraryFilter => {
            serde_json::json!({ "operation": "clear-library-filter" })
        }
        BrowserOperation::ShowSettings => serde_json::json!({ "operation": "show-settings" }),
        BrowserOperation::ShowPage => serde_json::json!({ "operation": "show-page" }),
        BrowserOperation::ClearHistory => serde_json::json!({ "operation": "clear-history" }),
        BrowserOperation::RequestWebsiteDataClear => {
            serde_json::json!({ "operation": "request-website-data-clear" })
        }
        BrowserOperation::TogglePictureInPicture => {
            serde_json::json!({ "operation": "toggle-picture-in-picture" })
        }
        BrowserOperation::ToggleHistoryPause => {
            serde_json::json!({ "operation": "toggle-history-pause" })
        }
        BrowserOperation::StartDownload { url } => {
            serde_json::json!({ "operation": "start-download", "url": url })
        }
        BrowserOperation::CancelDownload { id } => {
            serde_json::json!({ "operation": "cancel-download", "id": id })
        }
        BrowserOperation::RetryDownload { id } => {
            serde_json::json!({ "operation": "retry-download", "id": id })
        }
        BrowserOperation::OpenDownload { id } => {
            serde_json::json!({ "operation": "open-download", "id": id })
        }
        BrowserOperation::RevealDownload { id } => {
            serde_json::json!({ "operation": "reveal-download", "id": id })
        }
    }
}

fn encode_browser_operation(operation: &BrowserOperation) -> EncodedAction {
    let payload = browser_operation_payload(operation);
    EncodedAction::encode_operation(BROWSER_OPERATION_TYPE, &payload).unwrap_or_else(|_| {
        EncodedAction {
            type_id: BROWSER_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

fn decode_browser_operation(payload: &[u8]) -> Result<BrowserOperation, String> {
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
    match value.get("operation").and_then(serde_json::Value::as_str) {
        Some("back") => Ok(BrowserOperation::Back),
        Some("forward") => Ok(BrowserOperation::Forward),
        Some("reload") => Ok(BrowserOperation::Reload),
        Some("navigate") => value
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(|url| BrowserOperation::Navigate {
                url: url.to_string(),
            })
            .ok_or_else(|| "browser navigate operation is missing `url`".to_string()),
        Some("show-library") => {
            let section = match value.get("section").and_then(serde_json::Value::as_str) {
                Some("history") => LibrarySection::History,
                Some("bookmarks") => LibrarySection::Bookmarks,
                Some("downloads") => LibrarySection::Downloads,
                Some("extensions") => LibrarySection::Extensions,
                _ => return Err("browser library operation has an unknown section".into()),
            };
            Ok(BrowserOperation::ShowLibrary { section })
        }
        Some("set-library-filter") => required_string(&value, "query")
            .map(|query| BrowserOperation::SetLibraryFilter { query }),
        Some("clear-library-filter") => Ok(BrowserOperation::ClearLibraryFilter),
        Some("show-settings") => Ok(BrowserOperation::ShowSettings),
        Some("show-page") => Ok(BrowserOperation::ShowPage),
        Some("clear-history") => Ok(BrowserOperation::ClearHistory),
        Some("request-website-data-clear") => Ok(BrowserOperation::RequestWebsiteDataClear),
        Some("toggle-picture-in-picture") => Ok(BrowserOperation::TogglePictureInPicture),
        Some("toggle-history-pause") => Ok(BrowserOperation::ToggleHistoryPause),
        Some("start-download") => {
            required_string(&value, "url").map(|url| BrowserOperation::StartDownload { url })
        }
        Some("cancel-download") => {
            required_string(&value, "id").map(|id| BrowserOperation::CancelDownload { id })
        }
        Some("retry-download") => {
            required_string(&value, "id").map(|id| BrowserOperation::RetryDownload { id })
        }
        Some("open-download") => {
            required_string(&value, "id").map(|id| BrowserOperation::OpenDownload { id })
        }
        Some("reveal-download") => {
            required_string(&value, "id").map(|id| BrowserOperation::RevealDownload { id })
        }
        _ => Err("unknown browser operation".to_string()),
    }
}

fn required_string(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("browser operation is missing `{key}`"))
}

fn navigation_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        execution: ui::tilo_actions::ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "string" }),
        },
        tags: vec!["network".into(), "navigation".into()],
        ..ActionContract::view()
    }
}

/// An explicit contract for sensitive work that happens only in this running
/// browser profile.  These effects are intentionally not CRDT mutations: a
/// peer must not clear this machine's cookies, history, media surface, or
/// history-capture preference by replaying a collaborative document action.
fn local_effect_contract(tags: &[&str]) -> ActionContract {
    ActionContract {
        retry: RetryPolicy::AtMostOnce,
        reversible: false,
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "string" }),
        },
        tags: tags.iter().map(|tag| (*tag).into()).collect(),
        ..ActionContract::default()
    }
}

fn local_background_contract(tags: &[&str]) -> ActionContract {
    ActionContract {
        execution: ui::tilo_actions::ExecutionMode::Background { cancellable: false },
        ..local_effect_contract(tags)
    }
}

fn local_output(message: impl Into<String>) -> ui::tilo_actions::ActionOutput {
    ui::tilo_actions::ActionOutput {
        value: Some(serde_json::Value::String(message.into())),
        resources: Vec::new(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn download_status_text(entry: &crate::downloads::DownloadEntry) -> String {
    match entry.status {
        DownloadStatus::AwaitingDestination => "Waiting for a save destination…".into(),
        DownloadStatus::Downloading => match entry.total_bytes {
            Some(total) => format!(
                "{} of {} · {:.0}%",
                format_bytes(entry.received_bytes),
                format_bytes(total),
                entry.progress * 100.0
            ),
            None => format!("{} downloaded", format_bytes(entry.received_bytes)),
        },
        DownloadStatus::Completed => entry.destination.as_deref().map_or_else(
            || "Download complete".into(),
            |path| format!("Saved to {}", path.display()),
        ),
        DownloadStatus::Failed => entry
            .error
            .as_deref()
            .unwrap_or("The download failed")
            .to_string(),
        DownloadStatus::Cancelled => "Cancelled".into(),
    }
}

/// First page loaded in a fresh live window.
const DEFAULT_URL: &str = "https://duckduckgo.com";
/// Page loaded in `--shots` mode — the test engine renders it as a
/// deterministic flat colour, so snapshots stay byte-stable.
const SHOTS_URL: &str = "https://example.org";

/// Root widget for the browser app. See module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (bootstrap/headless \
              mode, engine-reported nav state, settings-pane visibility, dirty \
              gates); grouping them into enums would not express any real \
              invariant between them and would touch every read/write site \
              across this file"
)]
pub struct BrowserWidget {
    rect: Rect,
    engine: SharedEngine,
    composed: Option<Box<dyn Widget>>,
    bootstrap_done: bool,
    /// `--shots` mode: no network, no filter loader, deterministic page.
    headless: bool,
    initial_url: String,
    /// An address draft belongs to this window. Only an explicit Navigate
    /// resource operation crosses the browser/engine boundary.
    address: ui::LocalState<String>,

    // Chrome state mirrored from engine events.
    can_go_back: bool,
    can_go_forward: bool,
    /// `1.0` when idle; anything below shows the progress bar.
    progress: f64,
    title: String,
    /// Most recent URI the engine reported — recorded into history on
    /// load-finished.
    current_uri: String,
    load_error: Option<String>,
    /// A locally rejected address must outlive any late event from an earlier
    /// engine navigation. `WebView` events carry no request id, so this gate is
    /// deliberately owned by the chrome and cleared only when the user starts
    /// a fresh, allowed navigation.
    address_error_active: bool,
    forced_load_error: bool,
    forced_loading: bool,

    // User-facing browser state.
    settings: Settings,
    settings_presentation: SettingsPresentation,
    /// History pausing is an in-memory, device-local session boundary. It is
    /// never serialized to the collaborative document or the profile.
    history_paused: bool,
    /// Local presentation error after a history-clear write failed. Kept in
    /// the browser (rather than the shared document) because it describes
    /// this profile's disk, not collaborative state.
    history_clear_error: Option<String>,
    /// Tiny persistence seam: resource completion must mean the durable
    /// history write succeeded, and tests need to prove the failure path.
    history_save: fn(&History) -> std::io::Result<()>,
    history: History,
    bookmarks: Bookmarks,
    downloads: Downloads,
    download_picker: Option<(String, ui::portal::PortalChannel)>,
    download_picker_queue: std::collections::VecDeque<(String, String)>,
    download_save_due: Option<std::time::Instant>,
    /// Retained full-corpus index. Provider handles are cheap and unchanged
    /// frames compare only the durable corpus revision tokens.
    search_index: crate::search_provider::BrowserSearchIndex,
    /// Fuzzy suggestion dropdown under the address bar (a field, not a
    /// tree child: driven here and painted in `draw_overlay`).
    suggestions: AddressSuggestions,
    /// Whether the settings pane replaces the page surface.
    show_settings: bool,
    show_library: bool,
    library_section: LibrarySection,
    /// A single local query follows the reader between Library sections. It
    /// is intentionally not collaborative: filtering is a view preference,
    /// not a change to the user's history, bookmarks, or downloads.
    library_filter: ui::LocalState<String>,
    /// Set when `show_settings` flips, so the host rebuilds the tree.
    structure_dirty: bool,
    /// Last address-bar text a suggestion query ran for, to avoid
    /// re-querying every frame.
    last_query: Option<String>,
    /// Debug/`--shots-overlay`: keep forced suggestions visible without a
    /// real focus + query, so the overlay can be rendered headlessly.
    force_suggestions: bool,
    /// The `WebView` content rect as window fractions `[x0,y0,x1,y1]`,
    /// published each layout for the zero-copy compositor to blit into.
    content_rect: std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>>,
    resource_navigation_invocation: Option<ui::tilo_actions::InvocationId>,
    resource_navigation_result: Option<ui::tilo_actions::InvocationUpdate>,
    resource_navigation_deadline: Option<std::time::Instant>,
    resource_pip_invocation: Option<ui::tilo_actions::InvocationId>,
    resource_pip_request: Option<u64>,
    resource_pip_result: Option<ui::tilo_actions::InvocationUpdate>,
    resource_pip_deadline: Option<std::time::Instant>,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    pending_pip_region_update: Option<Rect>,
    published_pip_region: Option<Rect>,
    active_pip_element: Option<MediaElementInfo>,
    /// The no-video fallback mirrors the live page viewport instead of a
    /// semantic media element. Kept explicit so media polling never mistakes
    /// it for a disappeared `<video>` and closes it a quarter-second later.
    pip_page_fallback: bool,
    active_pip: bool,
    pip_request_seq: u64,
    active_pip_probe: Option<u64>,
    active_pip_refresh_probe: Option<u64>,
    next_pip_refresh_at: Option<std::time::Instant>,
    pip_feedback: Option<PipFeedback>,
    pip_feedback_rect: Rect,
    pip_float_page_rect: Rect,
    pip_select_area_rect: Rect,
    pending_capture_studio: Option<ui::widget::CaptureMode>,
    cursor: Point,
    compact_chrome: bool,
    motion_reduced: bool,
    pending_controls: std::collections::VecDeque<ControlInvocation>,
}

impl BrowserWidget {
    /// Live-mode widget with the default engine (WPE `WebKit` where the
    /// backend is available, the deterministic test engine otherwise).
    pub fn new() -> Self {
        Self::with_engine(default_engine(), false)
    }

    /// Headless widget for `--shots`: test engine, pre-navigated to a
    /// deterministic page, never touches the network.
    pub fn new_headless() -> Self {
        let engine: SharedEngine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(SHOTS_URL.to_string()));
        let mut w = Self::with_engine(engine, true);
        w.initial_url = SHOTS_URL.into();
        w
    }

    /// Set the first URI loaded before the widget bootstraps. The binary uses
    /// this only after validating the URI against Browser's declared schemes.
    #[must_use]
    pub fn with_initial_uri(mut self, uri: String) -> Self {
        self.initial_url = uri;
        self
    }

    /// Handle to the engine this widget drives — the binary uses it to
    /// wire the engine's waker to the winit event-loop proxy.
    pub fn engine_handle(&self) -> SharedEngine {
        self.engine.clone()
    }

    /// Widget over an explicit engine — used by tests.
    pub fn with_engine(engine: SharedEngine, headless: bool) -> Self {
        // Persisted state; `load()` returns defaults/empty when absent, so
        // `--shots` runs are unaffected by (and don't write) real files.
        let settings = if headless {
            Settings::default()
        } else {
            Settings::load()
        };
        let history = if headless {
            History::default()
        } else {
            History::load()
        };
        let bookmarks = if headless {
            Bookmarks::default()
        } else {
            Bookmarks::load()
        };
        let downloads = if headless {
            Downloads::default()
        } else {
            Downloads::load()
        };
        let initial_url = if is_supported_web_url(&settings.homepage) {
            settings.homepage.clone()
        } else {
            DEFAULT_URL.into()
        };
        let settings_presentation = SettingsPresentation::from_settings(&settings);
        let search_index = crate::search_provider::BrowserSearchIndex::default();
        search_index.sync_all(&history, &bookmarks, &downloads);
        Self {
            rect: Rect::ZERO,
            engine,
            composed: None,
            bootstrap_done: false,
            headless,
            address: ui::LocalState::new(initial_url.clone()),
            initial_url,
            can_go_back: false,
            can_go_forward: false,
            progress: 1.0,
            title: String::new(),
            current_uri: String::new(),
            load_error: None,
            address_error_active: false,
            forced_load_error: false,
            forced_loading: false,
            settings,
            settings_presentation,
            history_paused: false,
            history_clear_error: None,
            history_save: History::save,
            history,
            bookmarks,
            downloads,
            download_picker: None,
            download_picker_queue: std::collections::VecDeque::new(),
            download_save_due: None,
            search_index,
            suggestions: AddressSuggestions::new(),
            show_settings: false,
            show_library: false,
            library_section: LibrarySection::History,
            library_filter: ui::LocalState::new(String::new()),
            structure_dirty: false,
            last_query: None,
            force_suggestions: false,
            content_rect: std::sync::Arc::new(std::sync::Mutex::new(None)),
            resource_navigation_invocation: None,
            resource_navigation_result: None,
            resource_navigation_deadline: None,
            resource_pip_invocation: None,
            resource_pip_request: None,
            resource_pip_result: None,
            resource_pip_deadline: None,
            pending_pip: None,
            pending_pip_region_update: None,
            published_pip_region: None,
            active_pip_element: None,
            pip_page_fallback: false,
            active_pip: false,
            pip_request_seq: 0,
            active_pip_probe: None,
            active_pip_refresh_probe: None,
            next_pip_refresh_at: None,
            pip_feedback: None,
            pip_feedback_rect: Rect::ZERO,
            pip_float_page_rect: Rect::ZERO,
            pip_select_area_rect: Rect::ZERO,
            pending_capture_studio: None,
            cursor: Point::ZERO,
            compact_chrome: false,
            motion_reduced: false,
            pending_controls: std::collections::VecDeque::new(),
        }
    }

    /// Test hook: pin a suggestion list into view (bypasses focus/query)
    /// so the overlay can be rendered headlessly for inspection.
    pub fn debug_force_suggestions(&mut self, items: Vec<crate::history::HistoryEntry>) {
        self.suggestions.set_suggestions(items);
        self.force_suggestions = true;
    }

    /// Headless visual-review hook: show the settled no-video guidance card.
    pub fn debug_show_empty_pip_feedback(&mut self) {
        self.pip_feedback = Some(PipFeedback::persistent(PipFeedbackKind::NoVideo));
    }

    /// Headless visual-review hook for the toolbar's adopted `PiP` state.
    pub fn debug_show_active_pip(&mut self) {
        self.active_pip = true;
        self.active_pip_element = Some(MediaElementInfo {
            element_id: "media:debug".into(),
            viewport_rect: [0.14, 0.16, 0.72, 0.62],
            intrinsic_size: [1_920, 1_080],
            label: "The active film".into(),
        });
    }

    /// Headless visual-review hook for the recoverable navigation error state.
    pub fn debug_show_load_error(&mut self) {
        self.current_uri = "https://offline.example.test".into();
        self.initial_url = self.current_uri.clone();
        // The offline state owns the address shown in visual review.  Without
        // this, the headless engine's normal startup address made the chrome
        // claim DuckDuckGo while the recovery panel described a different
        // failed destination.
        self.address.set(self.current_uri.clone());
        self.title = "Couldn’t open this page".into();
        self.load_error =
            Some("The site did not respond. Check your connection and try again.".into());
        self.forced_load_error = true;
    }

    /// Headless visual-review hook for the in-flight navigation state.
    pub fn debug_show_loading(&mut self) {
        self.current_uri = "https://loading.example.test".into();
        self.initial_url = self.current_uri.clone();
        self.address.set(self.current_uri.clone());
        self.title = "Opening loading.example.test".into();
        self.progress = 0.42;
        self.forced_loading = true;
    }

    /// Seed the responsive breakpoint before a single-frame offscreen render.
    pub fn debug_set_viewport_width(&mut self, width: u32) {
        self.compact_chrome = width < 600;
        // The desktop Library cap also needs the viewport before its first
        // retained layout pass. Production windows already have this rect by
        // the time a user opens Library; headless review does not.
        self.rect = Rect::new(0.0, 0.0, f64::from(width), 0.0);
    }

    /// Populate and open a browser-library section for deterministic review.
    pub fn debug_show_library(&mut self, section: &str) {
        self.history
            .record_at("https://www.rust-lang.org/learn", "Learn Rust", 3);
        self.history.record_at(
            "https://developer.mozilla.org/en-US/docs/Web",
            "MDN Web Docs",
            2,
        );
        self.history
            .record_at("https://example.org/long-form", "Long-form reading", 1);
        let _ = self
            .bookmarks
            .add("https://www.rust-lang.org/learn", "Learn Rust");
        let _ = self
            .bookmarks
            .add("https://developer.mozilla.org/", "MDN Web Docs");
        self.library_section = match section {
            "bookmarks" => LibrarySection::Bookmarks,
            "downloads" => LibrarySection::Downloads,
            "extensions" => LibrarySection::Extensions,
            _ => LibrarySection::History,
        };
        self.show_library = true;
        self.show_settings = false;
    }

    /// Headless visual-review hook for a Library filter. This uses the same
    /// local state as typing and the canonical resource operation, so a shot
    /// can exercise the real empty- and matched-result layouts.
    pub fn debug_set_library_filter(&mut self, query: &str) {
        self.library_filter.set(bounded_library_filter(query));
        self.structure_dirty = true;
    }

    /// Deterministic download-model fixtures for visual review. These use the
    /// same lifecycle methods as live engine events rather than a parallel UI
    /// representation, so every captured row has a real resource/action shape.
    pub fn debug_show_downloads(&mut self, state: &str) {
        self.downloads = Downloads::default();
        match state {
            "empty" => {}
            "error" => {
                self.downloads.requested(
                    "review-failed".into(),
                    "https://example.org/archive.zip".into(),
                    "project-assets.zip",
                );
                self.downloads.failed(
                    "review-failed",
                    "The connection was interrupted. Your partial file was removed.".into(),
                    false,
                );
            }
            "completed" => {
                self.downloads.requested(
                    "review-complete".into(),
                    "https://example.org/guide.pdf".into(),
                    "Design-guide.pdf",
                );
                self.downloads.finished(
                    "review-complete",
                    "/home/alex/Downloads/Design-guide.pdf".into(),
                );
            }
            _ => {
                self.downloads.requested(
                    "review-progress".into(),
                    "https://example.org/tilo-sdk.tar.zst".into(),
                    "Tilo-SDK.tar.zst",
                );
                self.downloads.destination(
                    "review-progress",
                    "/home/alex/Downloads/Tilo-SDK.tar.zst".into(),
                );
                self.downloads.progress(
                    "review-progress",
                    503_316_480,
                    Some(1_073_741_824),
                    0.46875,
                );
            }
        }
        self.library_section = LibrarySection::Downloads;
        self.show_library = true;
        self.show_settings = false;
        self.structure_dirty = true;
    }

    pub fn debug_show_settings(&mut self) {
        self.show_settings = true;
        self.show_library = false;
    }

    /// Headless visual-review hook for the history-pause boundary. Existing
    /// entries deliberately remain visible: pausing recording is not a
    /// private-storage mode and must not hide prior durable history.
    pub fn debug_show_history_paused(&mut self) {
        if self.history.is_empty() {
            self.history
                .record_at("https://example.org/reading", "Existing reading", 2);
            self.history
                .record_at("https://www.rust-lang.org/learn", "Learn Rust", 1);
        }
        self.history_paused = true;
        self.show_library = true;
        self.show_settings = false;
        self.library_section = LibrarySection::History;
    }

    /// Real-engine integration hook used by `browser --shots-live --probe-pip`.
    /// It enters through the same probe/state machine as the toolbar button;
    /// only the synthetic pointer event is skipped.
    #[doc(hidden)]
    pub fn debug_request_picture_in_picture(&mut self) {
        self.begin_pip_probe();
    }

    /// Handle to the live `WebView` content rect (window fractions), for the
    /// zero-copy compositor. Updated every layout.
    pub fn content_rect_handle(&self) -> std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>> {
        self.content_rect.clone()
    }

    /// Perform a browser operation and report inputs that the Browser is not
    /// permitted to hand to the web engine.  In particular, a browser window
    /// is not a file picker: its package declares only the web schemes, and
    /// passing `file:` through would bypass the portal boundary.
    fn perform_browser_operation(&mut self, operation: BrowserOperation) -> Result<(), String> {
        match operation {
            BrowserOperation::Back => {
                self.clear_address_error();
                self.engine.send(WebViewCommand::GoBack);
            }
            BrowserOperation::Forward => {
                self.clear_address_error();
                self.engine.send(WebViewCommand::GoForward);
            }
            BrowserOperation::Reload => {
                self.clear_address_error();
                self.engine.send(WebViewCommand::Reload);
            }
            BrowserOperation::Navigate { url } => {
                let target = self.navigation_target(url.trim())?;
                self.clear_address_error();
                self.show_library = false;
                self.show_settings = false;
                self.structure_dirty = true;
                self.engine.send(WebViewCommand::Navigate(target));
            }
            BrowserOperation::ShowLibrary { section } => {
                self.library_section = section;
                self.show_library = true;
                self.show_settings = false;
                self.structure_dirty = true;
            }
            BrowserOperation::SetLibraryFilter { query } => {
                let query = query.trim();
                if query.len() > MAX_LIBRARY_FILTER_BYTES {
                    return Err(format!(
                        "library filter must be at most {MAX_LIBRARY_FILTER_BYTES} bytes"
                    ));
                }
                self.library_filter.set(query.to_string());
                self.show_library = true;
                self.show_settings = false;
                self.structure_dirty = true;
            }
            BrowserOperation::ClearLibraryFilter => {
                self.library_filter.set(String::new());
                self.structure_dirty = true;
            }
            BrowserOperation::ShowSettings => {
                self.show_settings = true;
                self.show_library = false;
                self.structure_dirty = true;
            }
            BrowserOperation::ShowPage => {
                self.show_settings = false;
                self.show_library = false;
                self.structure_dirty = true;
            }
            BrowserOperation::ClearHistory => {
                self.clear_history_durably()?;
            }
            BrowserOperation::RequestWebsiteDataClear => {
                self.engine.send(WebViewCommand::ClearWebsiteData);
            }
            BrowserOperation::TogglePictureInPicture => self.begin_pip_probe(),
            BrowserOperation::ToggleHistoryPause => {
                self.history_paused = !self.history_paused;
                self.structure_dirty = true;
            }
            BrowserOperation::StartDownload { url } => {
                let url = url.trim();
                if !is_supported_web_url(url) {
                    return Err("downloads require an absolute http:// or https:// URL".into());
                }
                self.engine.send(WebViewCommand::StartDownload {
                    uri: url.to_string(),
                });
            }
            BrowserOperation::CancelDownload { id } => {
                let entry = self
                    .downloads
                    .get(&id)
                    .ok_or_else(|| "download no longer exists".to_string())?;
                if !matches!(
                    entry.status,
                    DownloadStatus::AwaitingDestination | DownloadStatus::Downloading
                ) {
                    return Err("download is not active".into());
                }
                self.engine.send(WebViewCommand::CancelDownload { id });
            }
            BrowserOperation::RetryDownload { id } => {
                let entry = self
                    .downloads
                    .get(&id)
                    .ok_or_else(|| "download no longer exists".to_string())?;
                if !matches!(
                    entry.status,
                    DownloadStatus::Failed | DownloadStatus::Cancelled
                ) {
                    return Err("only a failed or cancelled download can be retried".into());
                }
                self.engine.send(WebViewCommand::StartDownload {
                    uri: entry.source_url.clone(),
                });
            }
            BrowserOperation::OpenDownload { id } => {
                let path = self.completed_download_path(&id)?;
                let _ = ui::portal::LaunchRequest::open(path)
                    .thread_name("browser-open-download")
                    .send();
            }
            BrowserOperation::RevealDownload { id } => {
                let path = self.completed_download_path(&id)?;
                let _ = ui::portal::LaunchRequest::reveal(path)
                    .thread_name("browser-reveal-download")
                    .send();
            }
        }
        Ok(())
    }

    fn completed_download_path(&self, id: &str) -> Result<std::path::PathBuf, String> {
        let entry = self
            .downloads
            .get(id)
            .ok_or_else(|| "download no longer exists".to_string())?;
        if entry.status != DownloadStatus::Completed {
            return Err("download has not completed".into());
        }
        entry
            .destination
            .clone()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| "download destination is unavailable".into())
    }

    /// Clear the in-memory corpus only when its new empty state is durable.
    /// A failed write restores the prior entries so actions/history never
    /// report local data as cleared when it will reappear on restart.
    fn clear_history_durably(&mut self) -> Result<(), String> {
        let previous = self.history.clone();
        self.history.clear();
        if !self.headless
            && let Err(error) = (self.history_save)(&self.history)
        {
            self.history = previous;
            let message =
                "Couldn’t clear history because Browser could not save the change. Previous history was kept."
                    .to_string();
            tracing::warn!("browser history clear was not persisted: {error}");
            self.history_clear_error = Some(message.clone());
            self.structure_dirty = true;
            return Err(message);
        }
        self.suggestions.clear();
        self.last_query = None;
        self.history_clear_error = None;
        self.structure_dirty = true;
        Ok(())
    }

    fn resource_by_id(&self, wanted: &str) -> Option<Resource> {
        fn walk(resources: &[Resource], wanted: &str) -> Option<Resource> {
            resources.iter().find_map(|resource| {
                (resource.id.0 == wanted)
                    .then(|| resource.clone())
                    .or_else(|| walk(&resource.children, wanted))
            })
        }
        walk(&self.resources(&AutoCommit::new()), wanted)
    }

    fn unit_binding(&self, resource: &str, action: &str) -> Option<ActionBinding<()>> {
        let resource = self.resource_by_id(resource)?;
        ActionBinding::unit_for(&resource, action).ok()
    }

    fn boolean_binding(
        &self,
        resource: &str,
        action: &str,
        parameter: &'static str,
    ) -> Option<ActionBinding<bool>> {
        let resource = self.resource_by_id(resource)?;
        let spec = resource.actions.iter().find(|spec| spec.id == action)?;
        ActionBinding::new(&resource, spec, move |value| {
            let mut arguments = serde_json::Map::new();
            arguments.insert(parameter.into(), serde_json::Value::Bool(value));
            arguments
        })
        .ok()
    }

    fn text_binding(
        &self,
        resource: &str,
        action: &str,
        parameter: &'static str,
    ) -> Option<ActionBinding<String>> {
        let resource = self.resource_by_id(resource)?;
        let spec = resource.actions.iter().find(|spec| spec.id == action)?;
        ActionBinding::new(&resource, spec, move |value| {
            let mut arguments = serde_json::Map::new();
            arguments.insert(parameter.into(), serde_json::Value::String(value));
            arguments
        })
        .ok()
    }

    fn settings_action_bindings(&self) -> SettingsActionBindings {
        SettingsActionBindings {
            adblock: self.boolean_binding("browser.settings", "set-adblock", "enabled"),
            clear_on_exit: self.boolean_binding("browser.settings", "set-clear-on-exit", "enabled"),
            homepage: self.text_binding("browser.settings", "set-homepage", "url"),
            search_engine: self.text_binding("browser.settings", "set-search-engine", "template"),
            clear_history: self.unit_binding("browser.history", "clear"),
            clear_site_data: self.unit_binding("browser.settings", "request-site-data-clear"),
            close: self.unit_binding("browser", "show-page"),
        }
    }

    fn bind_button(&self, button: Button, resource: &str, action: &str) -> Button {
        match self.unit_binding(resource, action) {
            Some(binding) => button.on_action(binding),
            None => button,
        }
    }

    /// Route a keyboard equivalent through the same canonical resource action
    /// as the corresponding toolbar control.  Focus movement is local UI
    /// state; navigation itself remains observable/replayable to the host.
    fn queue_unit_operation(&mut self, resource: &str, action: &str) {
        if let Some(binding) = self.unit_binding(resource, action)
            && let Ok(invocation) = binding.invoke(())
        {
            self.pending_controls.push_back(invocation);
        }
    }

    fn queue_navigate(&mut self, value: String) {
        let Some(resource) = self.resource_by_id("browser.page") else {
            return;
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("url".into(), serde_json::Value::String(value));
        if let Ok(binding) = ActionBinding::static_json_for(&resource, "navigate", arguments)
            && let Ok(invocation) = binding.invoke(())
        {
            self.pending_controls.push_back(invocation);
        }
    }

    /// Convert an address-bar value into a safe engine target. The engine is
    /// intentionally restricted to the schemes declared in `tilo-app.toml`;
    /// local files and internal `WebKit` schemes must be reached through their
    /// dedicated, consented system routes instead.
    fn navigation_target(&mut self, input: &str) -> Result<String, String> {
        let input = input.trim();
        if input.is_empty() {
            return self.reject_navigation("Enter a web address or search terms.");
        }
        if input.len() > 8 * 1024 || input.chars().any(char::is_control) {
            return self.reject_navigation("That address is not valid.");
        }

        // An explicitly schemed address must satisfy the manifest boundary.
        // We test it before treating dotted text as a host so `file:///a.html`
        // cannot get rewritten into an apparent HTTPS address.
        if input.contains("://") {
            let Ok(url) = url::Url::parse(input) else {
                return self.reject_navigation("That web address is malformed.");
            };
            if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
                return self
                    .reject_navigation("Browser can open only http:// or https:// addresses.");
            }
            return Ok(input.to_string());
        }

        if let Ok(url) = url::Url::parse(input)
            && !matches!(url.scheme(), "http" | "https")
            // URL parsing regards a bare `localhost:3000` / `127.0.0.1:3000`
            // as a scheme. Preserve these ordinary local-development web
            // addresses, while refusing `file:`, `about:`, `data:`, custom
            // schemes, and WebKit internals.
            && !looks_like_bare_web_host(input)
        {
            return self.reject_navigation("Browser can open only http:// or https:// addresses.");
        }

        let candidate = if !input.chars().any(char::is_whitespace)
            && looks_like_bare_web_host(input)
        {
            format!("https://{input}")
        } else {
            let query: String = url::form_urlencoded::byte_serialize(input.as_bytes()).collect();
            if self.settings.search_engine.contains(SEARCH_QUERY_TOKEN) {
                self.settings
                    .search_engine
                    .replace(SEARCH_QUERY_TOKEN, &query)
            } else {
                format!("{}{query}", self.settings.search_engine)
            }
        };
        let Ok(parsed) = url::Url::parse(&candidate) else {
            return self.reject_navigation("That web address is malformed.");
        };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
            return self.reject_navigation("Browser can open only http:// or https:// addresses.");
        }
        Ok(candidate)
    }

    fn reject_navigation<T>(&mut self, message: &str) -> Result<T, String> {
        self.show_navigation_error(message);
        Err(message.into())
    }

    fn show_navigation_error(&mut self, message: &str) {
        self.progress = 1.0;
        self.title = "Couldn’t open this address".into();
        self.load_error = Some(message.into());
        self.address_error_active = true;
        self.structure_dirty = true;
    }

    fn clear_address_error(&mut self) {
        self.address_error_active = false;
        if !self.forced_load_error {
            self.load_error = None;
        }
    }

    fn page_rect(&self) -> Rect {
        self.content_rect
            .lock()
            .ok()
            .and_then(|r| *r)
            .map_or(self.rect, |[x0, y0, x1, y1]| {
                Rect::new(
                    self.rect.x0 + f64::from(x0) * self.rect.width(),
                    self.rect.y0 + f64::from(y0) * self.rect.height(),
                    self.rect.x0 + f64::from(x1) * self.rect.width(),
                    self.rect.y0 + f64::from(y1) * self.rect.height(),
                )
            })
    }

    fn begin_pip_probe(&mut self) {
        if self.active_pip {
            self.active_pip = false;
            self.active_pip_element = None;
            self.pip_page_fallback = false;
            self.active_pip_refresh_probe = None;
            self.next_pip_refresh_at = None;
            self.pending_pip_region_update = None;
            self.published_pip_region = None;
            self.pip_feedback = None;
            self.structure_dirty = true;
            return;
        }
        self.pip_request_seq = self.pip_request_seq.saturating_add(1);
        self.active_pip_probe = Some(self.pip_request_seq);
        self.pip_feedback = Some(PipFeedback::transient(PipFeedbackKind::Finding));
        self.engine.send(WebViewCommand::RequestMediaElement {
            request_id: self.pip_request_seq,
        });
    }

    fn accept_media_element(&mut self, request_id: u64, element: MediaElementInfo) {
        if self.active_pip_refresh_probe == Some(request_id) {
            self.active_pip_refresh_probe = None;
            let same_element = self
                .active_pip_element
                .as_ref()
                .is_some_and(|active| active.element_id == element.element_id);
            if !self.active_pip || !same_element {
                self.end_active_pip_after_source_loss();
                return;
            }
            self.active_pip_element = Some(element);
            self.refresh_active_pip_region();
            self.next_pip_refresh_at = Some(std::time::Instant::now() + PIP_ELEMENT_REFRESH);
            return;
        }
        if self.active_pip_probe != Some(request_id) {
            return;
        }
        self.active_pip_probe = None;
        self.pip_feedback = None;

        let region = self.media_element_region(&element);
        if region.width() < 1.0 || region.height() < 1.0 {
            self.pip_feedback = Some(PipFeedback::transient(PipFeedbackKind::NoVideo));
            self.finish_pip_operation_failure(
                request_id,
                "browser.picture_in_picture_unavailable",
                "The page did not expose a visible video for Picture in Picture.",
            );
            return;
        }

        let intrinsic_aspect =
            f64::from(element.intrinsic_size[0]) / f64::from(element.intrinsic_size[1].max(1));
        let aspect = if intrinsic_aspect.is_finite() {
            intrinsic_aspect.clamp(0.4, 2.8)
        } else {
            (region.width() / region.height()).clamp(0.4, 2.8)
        };
        let preferred_size = Self::preferred_pip_size(aspect);

        self.pending_pip = Some(ui::floating_surface::FloatingSurfaceRequest {
            id: PIP_ID.into(),
            title: element.label.clone(),
            role: ui::floating_surface::FloatingRole::PictureInPicture,
            source: ui::floating_surface::FloatingSource::AppElement {
                app_id: "so.tilo.browser".into(),
                element_id: element.element_id.clone(),
                region,
            },
            preferred_size,
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        });
        self.active_pip_element = Some(element);
        self.pip_page_fallback = false;
        self.published_pip_region = Some(region);
        self.active_pip = true;
        self.next_pip_refresh_at = Some(std::time::Instant::now() + PIP_ELEMENT_REFRESH);
        self.structure_dirty = true;
        self.finish_pip_operation_success(request_id, "Picture in Picture opened.");
    }

    fn poll_active_pip_element(&mut self) {
        if !self.active_pip || self.pip_page_fallback || self.active_pip_refresh_probe.is_some() {
            return;
        }
        let due = self
            .next_pip_refresh_at
            .is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if !due {
            return;
        }
        let Some(element_id) = self
            .active_pip_element
            .as_ref()
            .map(|element| element.element_id.clone())
        else {
            return;
        };
        self.pip_request_seq = self.pip_request_seq.saturating_add(1);
        self.active_pip_refresh_probe = Some(self.pip_request_seq);
        self.next_pip_refresh_at = Some(std::time::Instant::now() + PIP_ELEMENT_REFRESH);
        self.engine.send(WebViewCommand::RequestMediaElementById {
            request_id: self.pip_request_seq,
            element_id,
        });
    }

    fn end_active_pip_after_source_loss(&mut self) {
        self.active_pip = false;
        self.active_pip_element = None;
        self.pip_page_fallback = false;
        self.active_pip_refresh_probe = None;
        self.next_pip_refresh_at = None;
        self.pending_pip_region_update = None;
        self.published_pip_region = None;
        self.pip_feedback = Some(PipFeedback::transient(PipFeedbackKind::NoVideo));
        self.structure_dirty = true;
    }

    fn media_element_region(&self, element: &MediaElementInfo) -> Rect {
        let page = self.page_rect();
        let [x, y, width, height] = element.viewport_rect;
        Rect::new(
            page.x0 + x * page.width(),
            page.y0 + y * page.height(),
            page.x0 + (x + width).min(1.0) * page.width(),
            page.y0 + (y + height).min(1.0) * page.height(),
        )
    }

    fn refresh_active_pip_region(&mut self) {
        if !self.active_pip {
            return;
        }
        if self.pip_page_fallback {
            let region = self.page_rect();
            self.publish_changed_pip_region(region);
            return;
        }
        let Some(element) = self.active_pip_element.clone() else {
            return;
        };
        let region = self.media_element_region(&element);
        self.publish_changed_pip_region(region);
    }

    fn publish_changed_pip_region(&mut self, region: Rect) {
        let changed = self.published_pip_region.is_none_or(|previous| {
            (previous.x0 - region.x0).abs() > 0.25
                || (previous.y0 - region.y0).abs() > 0.25
                || (previous.x1 - region.x1).abs() > 0.25
                || (previous.y1 - region.y1).abs() > 0.25
        });
        if changed && region.area() > 1.0 {
            self.pending_pip_region_update = Some(region);
            self.published_pip_region = Some(region);
        }
    }

    fn preferred_pip_size(aspect: f64) -> vello::kurbo::Size {
        if aspect >= 1.0 {
            let width = (225.0 * aspect).clamp(360.0, 560.0);
            vello::kurbo::Size::new(width, width / aspect)
        } else {
            let height = (260.0 / aspect).min(520.0);
            vello::kurbo::Size::new(height * aspect, height)
        }
    }

    /// Useful, explicit fallback when the page has no visible `<video>`:
    /// float the page viewport itself. Arbitrary-area selection remains the
    /// shell capture studio's job and is named in the actionable card.
    fn float_current_page(&mut self) {
        let region = self.page_rect();
        if region.width() < 1.0 || region.height() < 1.0 {
            return;
        }
        let aspect = (region.width() / region.height()).clamp(0.4, 2.8);
        let title = if self.title.trim().is_empty() {
            "Browser page".to_string()
        } else {
            self.title.clone()
        };
        self.pending_pip = Some(ui::floating_surface::FloatingSurfaceRequest {
            id: PIP_ID.into(),
            title,
            role: ui::floating_surface::FloatingRole::PictureInPicture,
            source: ui::floating_surface::FloatingSource::AppRegion {
                app_id: "so.tilo.browser".into(),
                region,
            },
            preferred_size: Self::preferred_pip_size(aspect),
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        });
        self.active_pip = true;
        self.pip_page_fallback = true;
        self.active_pip_element = None;
        self.published_pip_region = Some(region);
        self.pending_pip_region_update = None;
        self.next_pip_refresh_at = None;
        self.pip_feedback = None;
        self.pip_feedback_rect = Rect::ZERO;
        self.pip_float_page_rect = Rect::ZERO;
        self.pip_select_area_rect = Rect::ZERO;
        self.structure_dirty = true;
    }

    fn reject_media_element(&mut self, request_id: u64) {
        if self.active_pip_refresh_probe == Some(request_id) {
            self.end_active_pip_after_source_loss();
            return;
        }
        if self.active_pip_probe != Some(request_id) {
            return;
        }
        self.active_pip_probe = None;
        self.pip_feedback = Some(PipFeedback::transient(PipFeedbackKind::NoVideo));
        self.finish_pip_operation_failure(
            request_id,
            "browser.picture_in_picture_unavailable",
            "No visible video is available for Picture in Picture.",
        );
    }

    fn finish_pip_operation_success(&mut self, request_id: u64, message: &str) {
        if self.resource_pip_request != Some(request_id) {
            return;
        }
        let Some(invocation) = self.resource_pip_invocation.take() else {
            return;
        };
        self.resource_pip_request = None;
        self.resource_pip_deadline = None;
        self.resource_pip_result = Some(ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: local_output(message),
        });
    }

    fn finish_pip_operation_failure(&mut self, request_id: u64, code: &str, message: &str) {
        if self.resource_pip_request != Some(request_id) {
            return;
        }
        let Some(invocation) = self.resource_pip_invocation.take() else {
            return;
        };
        self.resource_pip_request = None;
        self.resource_pip_deadline = None;
        self.resource_pip_result = Some(ui::tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: ui::tilo_actions::ActionFailure {
                code: code.into(),
                message: message.into(),
                details: None,
                retryable: false,
            },
        });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "The browser chrome is intentionally composed in one ordered layout function."
    )]
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        // Compact, square-ish icon buttons (44×40 — a comfortable touch
        // target) instead of the toolkit's 120×40 label-button default,
        // so three of them leave the address bar the rest of the width
        // even on a narrow / mobile-width window.
        let nav_button_height = (40.0 + 16.0 * (theme.text_scale - 1.0)).clamp(40.0, 64.0);
        let nav_button_size = (44.0, nav_button_height);
        let back = self.bind_button(
            Button::new("browser.back", "", None)
                .with_icon(Icon::ArrowLeft)
                .with_accessible_name("Back")
                .tooltip("Back (Alt+Left)")
                .with_size(nav_button_size.0, nav_button_size.1)
                .with_enabled(self.can_go_back),
            "browser.page",
            "back",
        );
        let forward = self.bind_button(
            Button::new("browser.forward", "", None)
                .with_icon(Icon::ArrowRight)
                .with_accessible_name("Forward")
                .tooltip("Forward (Alt+Right)")
                .with_size(nav_button_size.0, nav_button_size.1)
                .with_enabled(self.can_go_forward),
            "browser.page",
            "forward",
        );
        let reload = self.bind_button(
            Button::new("browser.reload", "", None)
                .with_icon(Icon::Refresh)
                .with_accessible_name("Reload")
                .tooltip("Reload (Ctrl+R)")
                .with_size(nav_button_size.0, nav_button_size.1),
            "browser.page",
            "reload",
        );
        let url_input = TextInput::from_state(&self.address)
            .with_kind(TextInputKind::Url)
            .with_id(URL_INPUT_ID)
            .with_accessible_name("Address and search")
            .with_placeholder("Search or enter address");
        // These two toolbar controls change meaning while their destination
        // surface is already open.  Bind that visible meaning directly to
        // the matching view action instead of leaving a misleading
        // "close" affordance wired to a `show` operation.
        let (settings_resource, settings_action) = if self.show_settings {
            ("browser", "show-page")
        } else {
            ("browser.settings", "show")
        };
        let settings_btn = self.bind_button(
            Button::new("browser.settings", "", None)
                .with_icon(Icon::Settings)
                .with_accessible_name(if self.show_settings {
                    "Close browser settings"
                } else {
                    "Open browser settings"
                })
                .tooltip(if self.show_settings {
                    "Close settings"
                } else {
                    "Browser settings"
                })
                .with_size(nav_button_size.0, nav_button_size.1),
            settings_resource,
            settings_action,
        );
        let pip_label = if self.active_pip {
            "Close Picture in Picture"
        } else {
            "Picture in Picture"
        };
        let pip_btn = self.bind_button(
            Button::new("browser.pip", "", None)
                .with_icon(Icon::PictureInPicture2)
                .with_accessible_name(pip_label)
                .tooltip(pip_label)
                .with_size(nav_button_size.0, nav_button_size.1),
            "browser.page",
            "picture-in-picture",
        );
        let pip_btn = if self.active_pip {
            pip_btn.primary()
        } else {
            pip_btn
        };
        let history_pause_label = if self.history_paused {
            "Resume history in this window"
        } else {
            "Pause history in this window"
        };
        let history_pause_btn = self.bind_button(
            Button::new("browser.history-pause", "", None)
                .with_icon(Icon::Shield)
                .with_accessible_name(history_pause_label)
                .tooltip(history_pause_label)
                .with_size(nav_button_size.0, nav_button_size.1),
            // The pause state is a local session resource.  Its *kind* is
            // `browser.history-pause`, but its stable resource identity is
            // `browser.session`; binding to the kind silently left this
            // keyboard/assistive-tech control without an action address.
            "browser.session",
            "toggle-history-pause",
        );
        let history_pause_btn = if self.history_paused {
            history_pause_btn.primary()
        } else {
            history_pause_btn
        };

        // `no_wrap` keeps the address input on the same line as the
        // buttons (a wrapping row would push the 100%-width input onto
        // its own line instead of shrinking it to the remaining space).
        let mut toolbar = Row::new()
            .no_wrap()
            .push(Box::new(back))
            .push(Box::new(reload));
        if !self.compact_chrome {
            toolbar = toolbar.push(Box::new(forward));
        }
        toolbar = toolbar.push(Box::new(url_input));
        let bookmark_label = if self.bookmarks.contains(&self.current_uri) {
            "Page bookmarked"
        } else {
            "Bookmark this page"
        };
        let bookmark_btn = self.bind_button(
            Button::new("browser.bookmark", "", None)
                .with_icon(Icon::Star)
                .with_accessible_name(bookmark_label)
                .tooltip(bookmark_label)
                .with_enabled(!self.current_uri.trim().is_empty())
                .with_size(nav_button_size.0, nav_button_size.1),
            "browser.page",
            "bookmark",
        );
        let (library_resource, library_action) = if self.show_library {
            ("browser", "show-page")
        } else {
            ("browser.history", "show")
        };
        let library_btn = self.bind_button(
            Button::new("browser.library", "", None)
                .with_icon(Icon::History)
                .with_accessible_name(if self.show_library {
                    "Close browser library"
                } else {
                    "Open browser library"
                })
                .tooltip("History, bookmarks and downloads")
                .with_size(nav_button_size.0, nav_button_size.1),
            library_resource,
            library_action,
        );
        // On a phone-width window the disabled bookmark affordance was
        // consuming the only useful address-bar width. Bookmarks remain in
        // the Library/resource surface; keep the toolbar focused on finding
        // or entering a destination and recovering history.
        if !self.compact_chrome {
            toolbar = toolbar.push(Box::new(bookmark_btn));
        }
        // Compact chrome protects the address field first.  History capture
        // remains available from the History section itself, where the
        // longer privacy label is understandable and touch-sized, instead
        // of reducing an address/search field to an unusable sliver.
        if !self.compact_chrome {
            toolbar = toolbar.push(Box::new(history_pause_btn));
        }
        toolbar = toolbar.push(Box::new(library_btn));
        if !self.compact_chrome {
            toolbar = toolbar.push(Box::new(pip_btn));
        }
        toolbar = toolbar.push(Box::new(settings_btn));

        // The web view must reach the window edges — no border, no gap.
        // Only the toolbar keeps an inset (its wrapping Container's
        // default padding); the outer column drops all padding and the
        // inter-child gap so the page sits flush under the toolbar and
        // fills the rest of the window.
        let mut column = Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(Container::new().with_child(Box::new(toolbar))));
        if self.history_paused {
            column = column.push(Box::new(
                Container::new().with_background().with_child(Box::new(
                    Text::new(
                        "History paused — pages in this window are not saved to browsing history.",
                    )
                    .with_size(13.0),
                )),
            ));
        }
        // The content region is either the settings pane or the live page.
        let content: Box<dyn Widget> = if self.show_settings {
            Box::new(
                SettingsPane::with_settings(&self.settings)
                    .with_presentation(self.settings_presentation.clone())
                    .with_action_bindings(self.settings_action_bindings())
                    .with_history_clear_error(self.history_clear_error.clone()),
            )
        } else if self.show_library {
            self.build_library_composition(theme)
        } else if let Some(error) = &self.load_error {
            let recovery = EmptyState::new("Couldn’t open this page")
                .with_icon(Icon::Wifi)
                .with_message(error)
                .with_action("Try again")
                .with_action_id("browser.retry");
            let recovery: Box<dyn Widget> =
                Box::new(match self.unit_binding("browser.page", "reload") {
                    Some(binding) => recovery.on_action(binding),
                    // Retain the no-registry fallback for standalone widget
                    // previews; hosted Browser always supplies the action above.
                    None => recovery,
                });
            // EmptyState grows into the space it is offered. Give recovery a
            // remaining-height pane rather than letting the outer chrome
            // column size it at its compact intrinsic height, which otherwise
            // makes large text shed the explanatory copy even on a tall phone.
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(0.0)
                    .with_fill_remaining_height()
                    .push(recovery),
            )
        } else {
            Box::new(WebView::new(self.engine.clone()).with_test_id("browser.surface"))
        };
        // A percentage-height WebView cannot be a third direct flex child:
        // with toolbar + progress it can claim the outer column's full
        // height and paint over the chrome.  Give loading content its own
        // remaining-height flex region so the fixed progress strip always
        // sits beneath the toolbar, above the page.
        let content = if !self.show_settings && self.progress < 1.0 {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(0.0)
                    .with_fill_remaining_height()
                    .push(Box::new(
                        ProgressBar::new(self.progress as f32).with_label("Page loading"),
                    ))
                    .push(content),
            ) as Box<dyn Widget>
        } else {
            content
        };
        column = column.push(content);
        Box::new(column)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "The library sections share one ordered composition for consistent focus traversal."
    )]
    #[allow(
        clippy::indexing_slicing,
        reason = "Each row list is explicitly capped to its matching fixed flag array."
    )]
    fn build_library_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let large_text = theme.text_scale >= 1.5;
        let compact_layout = self.compact_chrome || large_text;
        let action_height = if large_text { 56.0 } else { 36.0 };
        let section_width = if self.compact_chrome && large_text {
            ((self.rect.width() as f32 - 36.0) / 2.0).max(150.0)
        } else if large_text {
            220.0
        } else {
            132.0
        };
        // At intermediate desktop widths, retain one coherent panel but never
        // force a fixed 920px card past the viewport. Compact chrome remains
        // fully fluid below its own breakpoint.
        let desktop_content_width = if self.rect.width() > 0.0 {
            LIBRARY_DESKTOP_CONTENT_WIDTH.min((self.rect.width() as f32 - 48.0).max(0.0))
        } else {
            LIBRARY_DESKTOP_CONTENT_WIDTH
        };
        let mut heading = Row::new()
            .push(Box::new(
                Text::new("Library").with_size(24.0).with_heading_level(1),
            ))
            .push(Box::new(Spacer::new()));
        if self.compact_chrome && !large_text && self.library_section == LibrarySection::History {
            let label = if self.history_paused {
                "Resume history"
            } else {
                "Pause history"
            };
            heading = heading.push(Box::new(
                self.bind_button(
                    Button::new("browser.library.history-pause", label, None)
                        .with_accessible_name(label)
                        .with_size(116.0, action_height),
                    "browser.session",
                    "toggle-history-pause",
                ),
            ));
        }
        heading = heading.push(Box::new(
            self.bind_button(
                Button::new("browser.library.close", "Done", None)
                    .with_size(if large_text { 112.0 } else { 96.0 }, action_height),
                "browser",
                "show-page",
            ),
        ));
        let section_definitions = [
            (
                "browser.library.history",
                "History",
                self.library_section == LibrarySection::History,
                "browser.history",
            ),
            (
                "browser.library.bookmarks",
                "Bookmarks",
                self.library_section == LibrarySection::Bookmarks,
                "browser.bookmarks",
            ),
            (
                "browser.library.downloads",
                "Downloads",
                self.library_section == LibrarySection::Downloads,
                "browser.downloads",
            ),
            (
                "browser.library.extensions",
                "Extensions",
                self.library_section == LibrarySection::Extensions,
                "browser.extensions",
            ),
        ];
        let mut section_buttons = Vec::new();
        for (id, label, selected, resource) in section_definitions {
            let button = self.bind_button(
                Button::new(id, label, None)
                    .with_accessible_name(&format!("Show {label}"))
                    .with_size(section_width, if large_text { 56.0 } else { 40.0 }),
                resource,
                "show",
            );
            section_buttons.push(if selected { button.primary() } else { button });
        }
        // Four fixed-width desktop tabs used to overflow the 390px layout.
        // Keep the familiar one-line switcher on desktop, but use a balanced
        // two-by-two grid in compact chrome: every destination remains
        // visible, touch-sized, and in the same tab order without requiring
        // horizontal scrolling or clipped labels.
        let sections: Box<dyn Widget> = if compact_layout {
            let mut buttons = section_buttons.into_iter();
            let first = buttons.next();
            let second = buttons.next();
            let third = buttons.next();
            let fourth = buttons.next();
            let first_row = Row::new()
                .no_wrap()
                .push(Box::new(first.unwrap_or_else(|| {
                    Button::new("browser.library.history", "History", None)
                })))
                .push(Box::new(second.unwrap_or_else(|| {
                    Button::new("browser.library.bookmarks", "Bookmarks", None)
                })));
            let second_row = Row::new()
                .no_wrap()
                .push(Box::new(third.unwrap_or_else(|| {
                    Button::new("browser.library.downloads", "Downloads", None)
                })))
                .push(Box::new(fourth.unwrap_or_else(|| {
                    Button::new("browser.library.extensions", "Extensions", None)
                })));
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(first_row))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(8.0),
                    ))
                    .push(Box::new(second_row)),
            )
        } else {
            let mut row = Row::new();
            for button in section_buttons {
                row = row.push(Box::new(button));
            }
            Box::new(row)
        };

        let filter_query = self.library_filter.get();
        let section_label = self.library_section.label();
        let filter_input = TextInput::from_state(&self.library_filter)
            .with_id(LIBRARY_FILTER_INPUT_ID)
            .with_kind(TextInputKind::Search)
            .with_accessible_name(&format!("Filter {section_label}"))
            .with_placeholder(&format!("Filter {section_label}"));
        let clear_filter = self.bind_button(
            Button::new("browser.library.clear-filter", "Clear", None)
                .with_accessible_name("Clear library filter")
                .tooltip("Clear filter")
                .with_enabled(!filter_query.trim().is_empty())
                .with_size(if large_text { 112.0 } else { 76.0 }, action_height),
            "browser.library",
            "clear-filter",
        );
        let filter_bar: Box<dyn Widget> = if compact_layout && large_text {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(filter_input))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(8.0),
                    ))
                    .push(Box::new(clear_filter.fill_width(action_height))),
            )
        } else {
            Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(Box::new(filter_input)),
                    ))
                    .push(Box::new(clear_filter)),
            )
        };

        let body: Box<dyn Widget> = match self.library_section {
            LibrarySection::History if self.history.is_empty() => {
                let (title, message) = if self.compact_chrome {
                    if self.history_paused {
                        ("History paused", "New pages are not saved.")
                    } else {
                        ("No browsing history yet", "Pages you visit appear here.")
                    }
                } else {
                    if self.history_paused {
                        (
                            "History is paused in this window",
                            "New pages here aren’t saved to history.",
                        )
                    } else {
                        (
                            "No browsing history yet",
                            "Pages you visit appear here. History stays on this device.",
                        )
                    }
                };
                Box::new(
                    EmptyState::new(title)
                        .with_icon(Icon::Shield)
                        .with_message(message),
                )
            }
            LibrarySection::History => {
                let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                let mut matched = 0;
                for entry in self.history.entries().iter().rev().take(12) {
                    if !library_filter_matches(&filter_query, &[&entry.title, &entry.url]) {
                        continue;
                    }
                    matched += 1;
                    let resource_id = history_resource_id(&entry.url);
                    let title = if entry.title.trim().is_empty() {
                        entry.url.as_str()
                    } else {
                        entry.title.as_str()
                    };
                    let text_block: Box<dyn Widget> = Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .push(Box::new(Text::new(title).with_size(15.0)))
                            .push(Box::new(Text::new(&entry.url).with_size(12.0).dimmed())),
                    );
                    let open = self.bind_button(
                        Button::new(&format!("{resource_id}.open"), "Open", None)
                            .with_accessible_name(&format!("Open {title}"))
                            .tooltip("Open page")
                            .with_size(if large_text { 104.0 } else { 84.0 }, action_height),
                        &resource_id,
                        "open",
                    );
                    let row_content: Box<dyn Widget> = if self.compact_chrome && large_text {
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(text_block)
                                .push(Box::new(
                                    Container::new().with_no_padding().with_height(10.0),
                                ))
                                .push(Box::new(open.fill_width(action_height))),
                        )
                    } else {
                        Box::new(
                            Row::new()
                                .no_wrap()
                                .push(Box::new(
                                    Container::new()
                                        .with_no_padding()
                                        .with_flex_grow()
                                        .with_child(text_block),
                                ))
                                .push(Box::new(open)),
                        )
                    };
                    rows = rows
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_background()
                                .with_child(Box::new(
                                    Row::new()
                                        .no_wrap()
                                        .with_padding(14.0, 9.0)
                                        .push(row_content),
                                )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_height(8.0),
                        ));
                }
                if matched == 0 {
                    Box::new(
                        EmptyState::new("No history matches")
                            .with_icon(Icon::History)
                            .with_message("Try a different title, address, or keyword."),
                    )
                } else if compact_layout {
                    Box::new(rows)
                } else {
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(desktop_content_width)
                            .with_child(Box::new(rows)),
                    )
                }
            }
            LibrarySection::Bookmarks if self.bookmarks.entries().is_empty() => Box::new(
                EmptyState::new("No bookmarks yet")
                    .with_icon(Icon::Star)
                    .with_message("Use the star in the address bar to keep a page."),
            ),
            LibrarySection::Bookmarks => {
                let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                let mut matched = 0;
                for entry in self.bookmarks.entries().iter().take(12) {
                    if !library_filter_matches(&filter_query, &[&entry.title, &entry.url]) {
                        continue;
                    }
                    matched += 1;
                    // Use the same normalized identity as the resource
                    // surface. In particular, legacy JSON can contain a URL
                    // without its canonical trailing slash.
                    let resource_id = bookmark_resource_id(&entry.url);
                    let remove_control_id = format!("{resource_id}.remove");
                    let text_block: Box<dyn Widget> = Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .push(Box::new(Text::new(&entry.title).with_size(15.0)))
                            .push(Box::new(Text::new(&entry.url).with_size(12.0).dimmed())),
                    );
                    let open = self.bind_button(
                        Button::new(&format!("{resource_id}.open"), "Open", None)
                            .with_accessible_name(&format!("Open {}", entry.title))
                            .with_size(if large_text { 120.0 } else { 84.0 }, action_height),
                        &resource_id,
                        "open",
                    );
                    let remove = self.bind_button(
                        Button::new(&remove_control_id, "Remove", None)
                            .with_accessible_name(&format!("Remove bookmark {}", entry.title))
                            .tooltip("Remove bookmark")
                            .with_size(if large_text { 150.0 } else { 96.0 }, action_height),
                        &resource_id,
                        "remove",
                    );
                    let row_content: Box<dyn Widget> = if self.compact_chrome && large_text {
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(text_block)
                                .push(Box::new(
                                    Container::new().with_no_padding().with_height(10.0),
                                ))
                                .push(Box::new(
                                    Row::new()
                                        .no_wrap()
                                        .push(Box::new(open))
                                        .push(Box::new(remove)),
                                )),
                        )
                    } else {
                        Box::new(
                            Row::new()
                                .no_wrap()
                                .push(Box::new(
                                    Container::new()
                                        .with_no_padding()
                                        .with_flex_grow()
                                        .with_child(text_block),
                                ))
                                .push(Box::new(open))
                                .push(Box::new(remove)),
                        )
                    };
                    rows = rows
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_background()
                                .with_child(Box::new(
                                    Row::new()
                                        .no_wrap()
                                        .with_padding(14.0, 9.0)
                                        .push(row_content),
                                )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_height(8.0),
                        ));
                }
                if matched == 0 {
                    Box::new(
                        EmptyState::new("No bookmarks match")
                            .with_icon(Icon::Star)
                            .with_message("Try a different title, address, or keyword."),
                    )
                } else if compact_layout {
                    Box::new(rows)
                } else {
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(desktop_content_width)
                            .with_child(Box::new(rows)),
                    )
                }
            }
            LibrarySection::Downloads if self.downloads.entries().is_empty() => {
                if self.compact_chrome && large_text {
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_background()
                            .with_child(Box::new(
                                Column::new()
                                    .with_intrinsic_height()
                                    .push(Box::new(
                                        Text::new("No downloads yet")
                                            .with_size(18.0)
                                            .with_heading_level(2),
                                    ))
                                    .push(Box::new(
                                        Text::new("Files you download appear here. You choose where each one is saved.")
                                            .with_size(12.0)
                                            .dimmed(),
                                    )),
                            )),
                    )
                } else {
                    Box::new(
                        EmptyState::new("No downloads yet")
                            .with_icon(Icon::Download)
                            .with_message(
                                "Files you download appear here. You choose where each one is saved.",
                            ),
                    )
                }
            }
            LibrarySection::Downloads => {
                let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                let mut matched = 0;
                for entry in self.downloads.entries().iter().rev().take(40) {
                    let status = download_status_text(entry);
                    let display_name = entry.display_name();
                    if !library_filter_matches(
                        &filter_query,
                        &[&display_name, &entry.source_url, &status],
                    ) {
                        continue;
                    }
                    matched += 1;
                    let resource_id = entry.resource_id();
                    let mut details = Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(Text::new(entry.display_name()).with_size(15.0)))
                        .push(Box::new(Text::new(&status).with_size(12.0).dimmed()));
                    if entry.status == DownloadStatus::Downloading {
                        details = details
                            .push(Box::new(
                                Container::new().with_no_padding().with_height(8.0),
                            ))
                            .push(Box::new(
                                ProgressBar::new(entry.progress as f32)
                                    .with_label(format!("Downloading {}", entry.display_name())),
                            ));
                    }
                    let details: Box<dyn Widget> = Box::new(details);
                    let actions: Box<dyn Widget> = match entry.status {
                        DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => {
                            Box::new(
                                self.bind_button(
                                    Button::new(&format!("{resource_id}.cancel"), "Cancel", None)
                                        .with_accessible_name(&format!(
                                            "Cancel download {}",
                                            entry.display_name()
                                        ))
                                        .with_size(
                                            if large_text { 136.0 } else { 92.0 },
                                            action_height,
                                        ),
                                    &resource_id,
                                    "cancel",
                                ),
                            )
                        }
                        DownloadStatus::Completed => Box::new(
                            Row::new()
                                .no_wrap()
                                .push(Box::new(
                                    self.bind_button(
                                        Button::new(
                                            &format!("{resource_id}.reveal"),
                                            "Show in Files",
                                            None,
                                        )
                                        .with_accessible_name(&format!(
                                            "Show {} in Files",
                                            entry.display_name()
                                        ))
                                        .with_size(
                                            if large_text { 190.0 } else { 126.0 },
                                            action_height,
                                        ),
                                        &resource_id,
                                        "reveal",
                                    ),
                                ))
                                .push(Box::new(
                                    self.bind_button(
                                        Button::new(&format!("{resource_id}.open"), "Open", None)
                                            .with_accessible_name(&format!(
                                                "Open downloaded file {}",
                                                entry.display_name()
                                            ))
                                            .with_size(
                                                if large_text { 120.0 } else { 84.0 },
                                                action_height,
                                            ),
                                        &resource_id,
                                        "open",
                                    )
                                    .primary(),
                                )),
                        ),
                        DownloadStatus::Failed | DownloadStatus::Cancelled => Box::new(
                            self.bind_button(
                                Button::new(&format!("{resource_id}.retry"), "Retry", None)
                                    .with_accessible_name(&format!(
                                        "Retry download {}",
                                        entry.display_name()
                                    ))
                                    .with_size(
                                        if large_text { 128.0 } else { 88.0 },
                                        action_height,
                                    ),
                                &resource_id,
                                "retry",
                            )
                            .primary(),
                        ),
                    };
                    // `Row` fills its parent by design. Without a fixed
                    // action slot, this nested action row claims the full
                    // card width and leaves the flex-growing metadata column
                    // only a word wide. Keep the desktop actions compact and
                    // let the detail column own the remaining readable width.
                    let action_width = match entry.status {
                        DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => 92.0,
                        DownloadStatus::Completed => 126.0 + 84.0 + theme.gap_standard,
                        DownloadStatus::Failed | DownloadStatus::Cancelled => 88.0,
                    };
                    let content: Box<dyn Widget> = if large_text {
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(details)
                                .push(Box::new(
                                    Container::new().with_no_padding().with_height(12.0),
                                ))
                                .push(actions),
                        )
                    } else {
                        Box::new(
                            Row::new()
                                .no_wrap()
                                .push(Box::new(
                                    Container::new()
                                        .with_no_padding()
                                        .with_flex_grow()
                                        .with_child(details),
                                ))
                                .push(Box::new(
                                    Container::new()
                                        .with_no_padding()
                                        .with_width(action_width)
                                        .with_child(actions),
                                )),
                        )
                    };
                    rows = rows
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_background()
                                .with_child(Box::new(
                                    Row::new().no_wrap().with_padding(14.0, 11.0).push(content),
                                )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_height(8.0),
                        ));
                }
                if matched == 0 {
                    Box::new(
                        EmptyState::new("No downloads match")
                            .with_icon(Icon::Download)
                            .with_message("Try a different filename, address, or status."),
                    )
                } else if compact_layout {
                    Box::new(rows)
                } else {
                    Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(desktop_content_width)
                            .with_child(Box::new(rows)),
                    )
                }
            }
            LibrarySection::Extensions => {
                let installed = if self.headless {
                    Vec::new()
                } else {
                    crate::extensions::list_installed()
                };
                if installed.is_empty() {
                    Box::new(
                        EmptyState::new("No active extensions")
                            .with_icon(Icon::Layers)
                            .with_message(
                                "Extension packages can be inspected, but this WebKit build cannot activate them yet.",
                            ),
                    )
                } else {
                    let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                    let mut matched = 0;
                    for extension in installed.into_iter().take(100) {
                        if !library_filter_matches(
                            &filter_query,
                            &[&extension.name, &extension.id, &extension.version],
                        ) {
                            continue;
                        }
                        matched += 1;
                        let status = format!("Version {} · installed, inactive", extension.version);
                        rows = rows
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .with_background()
                                    .with_child(Box::new(
                                        Row::new().no_wrap().with_padding(14.0, 9.0).push(
                                            Box::new(
                                                Container::new()
                                                    .with_no_padding()
                                                    .with_flex_grow()
                                                    .with_child(Box::new(
                                                        Column::new()
                                                            .with_no_padding()
                                                            .with_intrinsic_height()
                                                            .push(Box::new(
                                                                Text::new(&extension.name)
                                                                    .with_size(15.0),
                                                            ))
                                                            .push(Box::new(
                                                                Text::new(&status)
                                                                    .with_size(12.0)
                                                                    .dimmed(),
                                                            )),
                                                    )),
                                            ),
                                        ),
                                    )),
                            ))
                            .push(Box::new(
                                Container::new().with_no_padding().with_height(8.0),
                            ));
                    }
                    if matched == 0 {
                        Box::new(
                            EmptyState::new("No extensions match")
                                .with_icon(Icon::Layers)
                                .with_message("Try a different name, identifier, or version."),
                        )
                    } else if compact_layout {
                        Box::new(rows)
                    } else {
                        Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_width(desktop_content_width)
                                .with_child(Box::new(rows)),
                        )
                    }
                }
            }
        };

        let mut content_column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(heading))
            .push(Box::new(
                Container::new().with_no_padding().with_height(18.0),
            ));
        if self.compact_chrome && large_text && self.library_section == LibrarySection::History {
            let label = if self.history_paused {
                "Resume history"
            } else {
                "Pause history"
            };
            content_column = content_column
                .push(Box::new(
                    self.bind_button(
                        Button::new("browser.library.history-pause", label, None)
                            .with_accessible_name(label)
                            .fill_width(action_height),
                        "browser.session",
                        "toggle-history-pause",
                    ),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(12.0),
                ));
        }
        let content: Box<dyn Widget> = Box::new(
            content_column
                .push(sections)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(14.0),
                ))
                .push(filter_bar)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(18.0),
                ))
                .push(body),
        );
        // Large text changes the *inside* of Library rows to keep their
        // controls readable and reachable, but it is not a phone-width
        // layout.  Keeping a desktop window's content measure at 200% avoids
        // turning three short history rows into edge-to-edge slabs with vast
        // empty space.  Only an actually compact viewport needs the fluid
        // full-width presentation.
        let content = if self.compact_chrome {
            content
        } else {
            Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(desktop_content_width)
                            .with_child(content),
                    ))
                    .push(Box::new(Spacer::new())),
            )
        };
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(Container::new().with_child(content))),
        )
    }

    fn sync_bookmarks(&mut self, state: &AutoCommit, _root: &ObjId) {
        let Some(incoming) = crate::actions::bookmarks_from_doc(state) else {
            return;
        };
        if incoming != self.bookmarks {
            self.bookmarks = incoming;
            self.search_index.invalidate();
            if !self.headless {
                let _ = self.bookmarks.save();
            }
            self.structure_dirty = true;
        }
    }

    /// Project only action-applied preference fields from the shared document
    /// into this process's browser engine and retained control mirrors. No UI
    /// event writes the document here: `BrowserAction` is the sole mutation
    /// path, preserving action history and collaboration attribution.
    fn sync_settings_from_document(&mut self, state: &AutoCommit, root: &ObjId) {
        fn boolean(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
            state
                .get(root, key)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_scalar().ok())
                .and_then(|value| value.to_bool())
        }
        fn text(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
            let (value, object) = state.get(root, key).ok()??;
            matches!(value, Value::Object(automerge::ObjType::Text))
                .then(|| state.text(&object).ok())
                .flatten()
        }

        let mut next = self.settings.clone();
        if let Some(enabled) = boolean(state, root, crate::settings::ADBLOCK_PATH) {
            next.adblock_enabled = enabled;
        }
        if let Some(enabled) = boolean(state, root, crate::settings::CLEAR_ON_EXIT_PATH) {
            next.clear_on_exit = enabled;
        }
        if let Some(value) = text(state, root, crate::settings::HOMEPAGE_PATH)
            && is_supported_web_url(&value)
        {
            next.homepage = value;
        }
        if let Some(value) = text(state, root, crate::settings::SEARCH_ENGINE_PATH)
            && is_supported_web_url(&value)
        {
            next.search_engine = value;
        }
        if next != self.settings {
            if next.adblock_enabled != self.settings.adblock_enabled {
                self.set_adblock(next.adblock_enabled);
            }
            self.settings = next;
            if !self.headless {
                self.settings.save();
            }
        }
        self.settings_presentation.reflect(&self.settings);
    }

    fn continuation_url(&self) -> Option<String> {
        let url = self.address.with(Clone::clone);
        let parsed = url::Url::parse(&url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return None;
        }
        // Query strings commonly carry one-shot invitation or login tokens.
        // A path-only continuation reopens the page without exporting them.
        let mut safe = parsed;
        safe.set_query(None);
        Some(safe.into())
    }

    /// Whether the address input currently holds keyboard focus (the
    /// user is editing — engine URI updates must not stomp their text).
    fn url_input_focused(&self) -> bool {
        fn walk(w: &dyn Widget) -> Option<bool> {
            if w.test_id() == Some(URL_INPUT_ID) {
                return Some(w.is_focused());
            }
            for c in w.children() {
                if let Some(f) = walk(c) {
                    return Some(f);
                }
            }
            None
        }
        self.composed
            .as_ref()
            .and_then(|c| walk(c.as_ref()))
            .unwrap_or(false)
    }

    /// Fold engine events into this window's chrome state. Engine state is a
    /// device-local external boundary, never a collaborative document write.
    fn apply_engine_events(&mut self) {
        if self
            .download_save_due
            .is_some_and(|due| std::time::Instant::now() >= due)
        {
            self.persist_downloads_now();
            self.download_save_due = None;
        }
        self.poll_download_picker();
        let editing = self.url_input_focused();
        for event in self.engine.drain_events() {
            match event {
                WebViewEvent::UriChanged(uri) => {
                    // Deterministic visual-audit states own their displayed
                    // address.  The headless engine is pre-navigated to the
                    // flat test page, and its bootstrap event must not make
                    // an offline/loading capture claim a different URL.
                    if self.forced_load_error || self.forced_loading {
                        continue;
                    }
                    if !self.forced_load_error && !self.address_error_active {
                        self.load_error = None;
                    }
                    if !editing {
                        self.address.set(uri.clone());
                    }
                    self.current_uri = uri;
                }
                WebViewEvent::TitleChanged(title)
                    if !self.forced_load_error && !self.forced_loading =>
                {
                    self.title = title;
                }
                WebViewEvent::LoadProgress(p) if !self.forced_loading => self.progress = p,
                WebViewEvent::TitleChanged(_) | WebViewEvent::LoadProgress(_) => {}
                WebViewEvent::LoadFinished => {
                    if !self.forced_load_error && !self.address_error_active {
                        self.load_error = None;
                    }
                    if !self.forced_loading {
                        self.progress = 1.0;
                    }
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if !self.headless
                        && !self.history_paused
                        && self.current_uri.starts_with("http")
                    {
                        self.history.record(&self.current_uri, &self.title);
                        let _ = self.history.save();
                    }
                    if let Some(invocation) = self.resource_navigation_invocation.take() {
                        self.resource_navigation_deadline = None;
                        self.resource_navigation_result =
                            Some(ui::tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: ui::tilo_actions::ActionOutput {
                                    value: Some(serde_json::Value::String(
                                        self.current_uri.clone(),
                                    )),
                                    resources: Vec::new(),
                                },
                            });
                    }
                }
                WebViewEvent::LoadFailed { uri, error } => {
                    self.progress = 1.0;
                    tracing::warn!("load failed for {uri}: {error}");
                    self.title = format!("Failed to load {uri}");
                    self.load_error = Some(error.chars().take(240).collect());
                    self.structure_dirty = true;
                    if let Some(invocation) = self.resource_navigation_invocation.take() {
                        self.resource_navigation_deadline = None;
                        self.resource_navigation_result =
                            Some(ui::tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: ui::tilo_actions::ActionFailure {
                                    code: "browser.navigation_failed".into(),
                                    message: error,
                                    details: Some(serde_json::json!({ "uri": uri })),
                                    retryable: true,
                                },
                            });
                    }
                }
                WebViewEvent::HistoryChanged {
                    can_go_back,
                    can_go_forward,
                } => {
                    self.can_go_back = can_go_back;
                    self.can_go_forward = can_go_forward;
                }
                WebViewEvent::MediaElement {
                    request_id,
                    element,
                } => self.accept_media_element(request_id, element),
                WebViewEvent::MediaElementUnavailable { request_id } => {
                    self.reject_media_element(request_id);
                }
                event @ (WebViewEvent::DownloadRequested { .. }
                | WebViewEvent::DownloadProgress { .. }
                | WebViewEvent::DownloadFinished { .. }
                | WebViewEvent::DownloadFailed { .. }) => self.apply_download_event(event),
            }
        }
    }

    fn apply_download_event(&mut self, event: WebViewEvent) {
        match event {
            WebViewEvent::DownloadRequested {
                id,
                uri,
                suggested_filename,
            } => {
                if !self
                    .downloads
                    .requested(id.clone(), uri, &suggested_filename)
                {
                    tracing::warn!("refusing duplicate or invalid engine download id {id}");
                    self.engine.send(WebViewCommand::CancelDownload { id });
                    return;
                }
                self.download_picker_queue
                    .push_back((id, suggested_filename));
                self.start_next_download_picker();
                self.persist_downloads_now();
                self.structure_dirty = true;
            }
            WebViewEvent::DownloadProgress {
                id,
                received_bytes,
                total_bytes,
                progress,
            } => {
                self.downloads
                    .progress(&id, received_bytes, total_bytes, progress);
                if self.download_save_due.is_none() {
                    self.download_save_due =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                }
                self.structure_dirty = true;
            }
            WebViewEvent::DownloadFinished { id, path } => {
                self.downloads.finished(&id, path.into());
                self.persist_downloads_now();
                self.download_save_due = None;
                self.structure_dirty = true;
            }
            WebViewEvent::DownloadFailed {
                id,
                error,
                cancelled,
            } => {
                self.downloads.failed(&id, error, cancelled);
                self.persist_downloads_now();
                self.download_save_due = None;
                self.structure_dirty = true;
            }
            _ => tracing::warn!("non-download event reached the download event handler"),
        }
    }

    fn persist_downloads_now(&self) {
        if !self.headless
            && let Err(error) = self.downloads.save()
        {
            tracing::warn!("browser download history was not persisted: {error}");
        }
    }

    fn start_next_download_picker(&mut self) {
        if self.download_picker.is_some() {
            return;
        }
        let Some((id, suggested_name)) = self.download_picker_queue.pop_front() else {
            return;
        };
        let channel = ui::portal::PickRequest::save("Save download", "Save", &suggested_name)
            .thread_name("browser-download-destination")
            .all_files()
            .send();
        self.download_picker = Some((id, channel));
    }

    fn poll_download_picker(&mut self) {
        let outcome =
            self.download_picker
                .as_ref()
                .and_then(|(id, channel)| match channel.try_recv() {
                    Ok(outcome) => Some((id.clone(), outcome)),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((
                        id.clone(),
                        ui::portal::PortalOutcome::Failed(
                            "The desktop save picker closed unexpectedly".into(),
                        ),
                    )),
                });
        let Some((id, outcome)) = outcome else {
            return;
        };
        self.download_picker = None;
        match outcome {
            ui::portal::PortalOutcome::Opened(path) if path.is_absolute() => {
                self.downloads.destination(&id, path.clone());
                self.engine.send(WebViewCommand::SetDownloadDestination {
                    id,
                    path: path.to_string_lossy().into_owned(),
                });
            }
            ui::portal::PortalOutcome::Opened(_) => {
                self.downloads.failed(
                    &id,
                    "The save destination was not an absolute path".into(),
                    false,
                );
                self.engine.send(WebViewCommand::CancelDownload { id });
            }
            ui::portal::PortalOutcome::Cancelled => {
                self.downloads.failed(&id, String::new(), true);
                self.engine.send(WebViewCommand::CancelDownload { id });
            }
            ui::portal::PortalOutcome::Failed(error) => {
                self.downloads.failed(&id, error, false);
                self.engine.send(WebViewCommand::CancelDownload { id });
            }
        }
        self.persist_downloads_now();
        self.download_save_due = None;
        self.structure_dirty = true;
        self.start_next_download_picker();
    }

    /// Navigate to whatever the address bar currently holds.
    fn navigate_to_input(&mut self) {
        let url = self.address.with(Clone::clone);
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            self.queue_navigate(trimmed.to_string());
        }
    }

    /// Enable or disable ad blocking on the engine (filter + scriptlets).
    fn set_adblock(&self, enabled: bool) {
        if enabled {
            crate::blocker::spawn_filter_loader(self.engine.clone());
            self.engine
                .send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
                    crate::blocker::user_scripts(),
                )));
        } else {
            self.engine.send(WebViewCommand::SetContentFilter(None));
            self.engine
                .send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
                    Vec::new(),
                )));
        }
    }

    /// Refresh the address-bar suggestion list from history while the
    /// address input is focused; hide it otherwise.
    fn update_suggestions(&mut self) {
        if self.force_suggestions {
            return; // headless overlay render — keep the pinned list
        }
        let text = if self.show_settings || !self.url_input_focused() {
            String::new()
        } else {
            self.address.with(Clone::clone)
        };
        let query = text.trim();
        // Only suggest while the user is editing a *query* — not when the
        // bar merely shows the loaded page's URL, and not for an empty bar.
        // (Echoing the current page back at the user is what made the old
        // overlay pointless.)
        let editing = !query.is_empty() && query != self.current_uri.trim();
        if !editing {
            if self.last_query.is_some() {
                self.suggestions.clear();
                self.last_query = None;
            }
            return;
        }
        if self.last_query.as_deref() == Some(query) {
            return;
        }
        self.last_query = Some(query.to_string());
        // Fuzzy history matches, minus the page we're already on.
        let items: Vec<_> = self
            .history
            .suggest(query, SUGGESTION_LIMIT)
            .into_iter()
            .filter(|e| e.url != self.current_uri)
            .collect();
        self.suggestions.set_suggestions(items);
    }

    /// Rect of the address `TextInput` in the composed tree, for anchoring
    /// the suggestion overlay under it.
    fn address_input_rect(&self) -> Option<Rect> {
        self.find_rect(URL_INPUT_ID)
    }

    /// `PiP` has a stable widget id.  Looking it up by position made the
    /// no-video guidance card anchor to a different control on compact
    /// chrome, where `PiP` intentionally is not part of the toolbar.
    fn pip_button_rect(&self) -> Option<Rect> {
        self.find_rect("browser.pip")
    }

    /// Anchor Picture-in-Picture feedback to the real trigger when it is
    /// visible. Compact chrome deliberately omits that low-frequency button;
    /// the settings affordance is its stable trailing-toolbar fallback so a
    /// no-video explanation remains visible (and clamped) rather than being
    /// silently dropped on phone-sized windows.
    fn pip_feedback_anchor(&self) -> Option<Rect> {
        self.pip_button_rect()
            .or_else(|| self.find_rect("browser.settings"))
            .or_else(|| self.address_input_rect())
    }

    /// Keep the transient `PiP` guidance card wholly inside the Browser's
    /// viewport. On compact chrome its logical anchor can sit near either
    /// edge, so anchoring only the card's trailing side would clip the text
    /// and first action offscreen.
    fn pip_feedback_rect_for_anchor(
        viewport: Rect,
        anchor: Rect,
        has_actions: bool,
    ) -> Option<Rect> {
        const INSET: f64 = 12.0;
        let available_width = viewport.width() - INSET * 2.0;
        if available_width <= 0.0 {
            return None;
        }
        let desired_width: f64 = if has_actions { 354.0 } else { 326.0 };
        let width = desired_width.min(available_width);
        let height = if has_actions { 112.0 } else { 64.0 };
        let left_bound = viewport.x0 + INSET;
        let right_bound = (viewport.x1 - INSET - width).max(left_bound);
        let x0 = (anchor.x1 + 2.0 - width).clamp(left_bound, right_bound);
        let top_bound = viewport.y0 + INSET;
        let bottom_bound = (viewport.y1 - INSET - height).max(top_bound);
        let y0 = (anchor.y1 + 8.0).clamp(top_bound, bottom_bound);
        Some(Rect::new(x0, y0, x0 + width, y0 + height))
    }

    fn sync_search_and_suggestions(&mut self) {
        self.search_index
            .sync_all(&self.history, &self.bookmarks, &self.downloads);
        self.update_suggestions();
    }

    fn handle_browser_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed || self.show_settings || self.show_library {
            return false;
        }
        let modifiers = modifiers.state();
        let alt_navigation =
            modifiers.alt_key() && !modifiers.control_key() && !modifiers.super_key();
        let reload = (modifiers.control_key() || modifiers.super_key())
            && !modifiers.alt_key()
            && matches!(&key.logical_key, WKey::Character(c) if c.eq_ignore_ascii_case("r"));
        let action = match (&key.logical_key, alt_navigation, reload) {
            (WKey::Named(WNamedKey::ArrowLeft), true, _) => Some("back"),
            (WKey::Named(WNamedKey::ArrowRight), true, _) => Some("forward"),
            (_, _, true) => Some("reload"),
            _ => None,
        };
        if let Some(action) = action {
            self.queue_unit_operation("browser.page", action);
            return true;
        }
        false
    }

    /// Browser-standard Ctrl/Cmd+L. Focus is kept inside the chrome, so a
    /// page cannot intercept the shortcut and leave keyboard users hunting
    /// for the address field.
    fn focus_address_input(&mut self) -> bool {
        self.focus_input(URL_INPUT_ID)
    }

    /// Move focus to one of Browser's retained local text controls. Keeping
    /// this traversal centralized gives the Library's Ctrl/Cmd+F the exact
    /// same predictable focus reset as Ctrl/Cmd+L.
    fn focus_input(&mut self, input_id: &str) -> bool {
        fn clear(widget: &mut dyn Widget) {
            widget.set_focus(false);
            for child in widget.children_mut() {
                clear(child);
            }
        }
        fn focus(widget: &mut dyn Widget, input_id: &str) -> bool {
            if widget.test_id() == Some(input_id) {
                widget.set_focus(true);
                return true;
            }
            for child in widget.children_mut() {
                if focus(child, input_id) {
                    return true;
                }
            }
            false
        }
        self.composed.as_mut().is_some_and(|composed| {
            clear(composed.as_mut());
            focus(composed.as_mut(), input_id)
        })
    }

    fn handle_library_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        let modifiers = modifiers.state();
        let requested = key.state == ElementState::Pressed
            && (modifiers.control_key() || modifiers.super_key())
            && !modifiers.alt_key()
            && matches!(&key.logical_key, WKey::Character(c) if c.eq_ignore_ascii_case("f"));
        requested && self.show_library && self.focus_input(LIBRARY_FILTER_INPUT_ID)
    }

    fn handle_address_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        let modifiers = modifiers.state();
        let requested = key.state == ElementState::Pressed
            && (modifiers.control_key() || modifiers.super_key())
            && !modifiers.alt_key()
            && matches!(&key.logical_key, WKey::Character(c) if c.eq_ignore_ascii_case("l"));
        requested && !self.show_settings && !self.show_library && self.focus_address_input()
    }

    /// Rect of the composed-tree widget with `test_id == id`, if present.
    fn find_rect(&self, id: &str) -> Option<Rect> {
        fn walk(w: &dyn Widget, id: &str) -> Option<Rect> {
            if w.test_id() == Some(id) {
                return Some(w.get_rect());
            }
            w.children().iter().find_map(|c| walk(*c, id))
        }
        self.composed.as_ref().and_then(|c| walk(c.as_ref(), id))
    }

    /// Publish the `WebView` surface rect as window fractions for the
    /// zero-copy compositor.
    fn publish_content_rect(&self) {
        let full = self.rect;
        if full.width() <= 0.0 || full.height() <= 0.0 {
            return;
        }
        let frac = self.find_rect("browser.surface").map(|r| {
            [
                ((r.x0 - full.x0) / full.width()) as f32,
                ((r.y0 - full.y0) / full.height()) as f32,
                ((r.x1 - full.x0) / full.width()) as f32,
                ((r.y1 - full.y0) / full.height()) as f32,
            ]
        });
        *self
            .content_rect
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = frac;
    }
}

impl Default for BrowserWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// The engine a live browser window runs on: WPE `WebKit` when the `wpe`
/// feature is compiled in (and its libraries boot), the deterministic
/// test engine otherwise — so the chrome stays fully drivable in
/// hermetic builds.
fn default_engine() -> SharedEngine {
    #[cfg(feature = "wpe")]
    {
        match ui::webview::wpe::WpeEngine::new() {
            Ok(engine) => return engine,
            Err(err) => tracing::warn!("WPE engine unavailable, using test engine: {err}"),
        }
    }
    TestEngine::new()
}

impl Continuable for BrowserWidget {
    fn continuation_metadata(
        &self,
        _state: &AutoCommit,
        _root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let url = self.continuation_url()?;
        let bytes = serde_json::to_vec(&serde_json::json!({ "url": url })).ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _state: &mut AutoCommit,
        _root: &ObjId,
    ) -> bool {
        if metadata.schema_version != 1 {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&metadata.bytes) else {
            return false;
        };
        let Some(url) = value.get("url").and_then(serde_json::Value::as_str) else {
            return false;
        };
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return false;
        }
        self.address.set(url.to_owned());
        url.clone_into(&mut self.initial_url);
        if self.bootstrap_done {
            self.engine.send(WebViewCommand::Navigate(url.to_owned()));
        }
        true
    }
}

impl FloatingSurfaceProvider for BrowserWidget {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_pip.take()
    }

    fn take_capture_studio_request(&mut self) -> Option<ui::widget::CaptureMode> {
        self.pending_capture_studio.take()
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.pending_pip_region_update
            .take()
            .map(|region| vec![(PIP_ID.into(), region)])
            .unwrap_or_default()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.active_pip.then(|| PIP_ID.into()).into_iter().collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        match message {
            ui::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. }
                if id == PIP_ID =>
            {
                self.active_pip = true;
            }
            ui::floating_surface::FloatingSurfaceHostMessage::Closed { id }
            | ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                if id == PIP_ID =>
            {
                self.active_pip = false;
                self.active_pip_element = None;
                self.pip_page_fallback = false;
                self.active_pip_refresh_probe = None;
                self.next_pip_refresh_at = None;
                self.pending_pip_region_update = None;
                self.published_pip_region = None;
                self.structure_dirty = true;
            }
            _ => {}
        }
    }
}

impl Widget for BrowserWidget {
    fn continuable(&self) -> Option<&dyn Continuable> {
        Some(self)
    }

    fn continuable_mut(&mut self) -> Option<&mut dyn Continuable> {
        Some(self)
    }

    fn floating_surface_provider(&self) -> Option<&dyn FloatingSurfaceProvider> {
        Some(self)
    }

    fn floating_surface_provider_mut(&mut self) -> Option<&mut dyn FloatingSurfaceProvider> {
        Some(self)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[BROWSER_OPERATION_TYPE]
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Resource definitions stay together so their action contracts remain discoverable."
    )]
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        const MATERIALIZED_LIMIT: usize = 100;
        let operation =
            |id: &str, label: &str, kind: OperationKind, operation: BrowserOperation| {
                ActionSpec::operation(
                    id,
                    label,
                    kind,
                    BROWSER_OPERATION_TYPE,
                    &browser_operation_payload(&operation),
                )
                .ok()
            };
        let mut page = Resource::new(
            "browser.page",
            "browser.page",
            if self.title.is_empty() {
                "Web page"
            } else {
                &self.title
            },
        );
        for (id, label, operation, available, reason) in [
            (
                "back",
                "Go back",
                BrowserOperation::Back,
                self.can_go_back,
                "No previous page",
            ),
            (
                "forward",
                "Go forward",
                BrowserOperation::Forward,
                self.can_go_forward,
                "No next page",
            ),
            ("reload", "Reload", BrowserOperation::Reload, true, ""),
        ] {
            let payload = browser_operation_payload(&operation);
            if let Ok(spec) = ActionSpec::operation(
                id,
                label,
                OperationKind::View,
                BROWSER_OPERATION_TYPE,
                &payload,
            ) {
                let spec = spec.with_contract(navigation_contract());
                page.actions.push(if available {
                    spec
                } else {
                    spec.unavailable("history_boundary", reason)
                });
            }
        }
        page.actions.push(
            ActionSpec::typed_operation(
                "navigate",
                "Open address",
                "https://…",
                OperationKind::Effect,
                BROWSER_OPERATION_TYPE,
                vec![ActionParam::required_owned(
                    "url",
                    ParamType::Text,
                    "Absolute URL or search terms",
                )],
                |args: &ActionArgs| {
                    encode_browser_operation(&BrowserOperation::Navigate {
                        url: args.text("url").unwrap_or("").to_string(),
                    })
                },
            )
            .with_contract(navigation_contract()),
        );
        if let Ok(spec) = ActionSpec::immediate(
            "bookmark",
            "Bookmark this page",
            BROWSER_ACTION_TYPE,
            &BrowserAction::AddBookmark {
                url: self.current_uri.clone(),
                title: self.title.clone(),
                tag: format!("bookmark:{}", uuid::Uuid::new_v4().simple()),
            },
        ) {
            page.actions.push(if self.current_uri.trim().is_empty() {
                spec.unavailable("no_page", "Open a page before bookmarking it")
            } else if self.bookmarks.contains(&self.current_uri) {
                spec.unavailable("already_bookmarked", "This page is already bookmarked")
            } else {
                spec.reversible()
            });
        }
        if let Some(spec) = operation(
            "picture-in-picture",
            if self.active_pip {
                "Close Picture in Picture"
            } else {
                "Open Picture in Picture"
            },
            OperationKind::View,
            BrowserOperation::TogglePictureInPicture,
        ) {
            page.actions
                .push(spec.with_contract(local_background_contract(&[
                    "device-local",
                    "session",
                    "media",
                ])));
        }

        let mut session = Resource::new(
            "browser.session",
            "browser.history-pause",
            if self.history_paused {
                "History paused"
            } else {
                "History recording"
            },
        );
        if let Some(spec) = operation(
            "toggle-history-pause",
            if self.history_paused {
                "Resume history in this window"
            } else {
                "Pause history in this window"
            },
            OperationKind::Effect,
            BrowserOperation::ToggleHistoryPause,
        ) {
            session
                .actions
                .push(spec.with_contract(local_effect_contract(&[
                    "device-local",
                    "session",
                    "history-paused",
                    "sensitive-data",
                ])));
        }

        let mut history = Resource::new("browser.history", "browser.history", "History");
        if let Some(spec) = operation(
            "show",
            "Show history",
            OperationKind::View,
            BrowserOperation::ShowLibrary {
                section: LibrarySection::History,
            },
        ) {
            history.actions.push(spec);
        }
        if let Some(spec) = operation(
            "clear",
            "Clear browsing history",
            OperationKind::Effect,
            BrowserOperation::ClearHistory,
        ) {
            history.actions.push(if self.history.is_empty() {
                spec.with_contract(local_effect_contract(&[
                    "device-local",
                    "history",
                    "sensitive-data",
                ]))
                .unavailable("already_empty", "Browsing history is already empty")
            } else {
                spec.with_contract(local_effect_contract(&[
                    "device-local",
                    "history",
                    "sensitive-data",
                ]))
                .destructive()
            });
        }
        for entry in self.history.entries().iter().rev().take(MATERIALIZED_LIMIT) {
            let title = if entry.title.trim().is_empty() {
                entry.url.as_str()
            } else {
                entry.title.as_str()
            };
            let mut child = Resource::new(
                history_resource_id(&entry.url),
                "browser.history-entry",
                title,
            );
            if let Some(spec) = operation(
                "open",
                "Open page",
                OperationKind::Effect,
                BrowserOperation::Navigate {
                    url: entry.url.clone(),
                },
            ) {
                child
                    .actions
                    .push(spec.with_contract(navigation_contract()));
            }
            history.children.push(child);
        }

        let mut bookmarks = Resource::new("browser.bookmarks", "browser.bookmarks", "Bookmarks");
        if let Some(spec) = operation(
            "show",
            "Show bookmarks",
            OperationKind::View,
            BrowserOperation::ShowLibrary {
                section: LibrarySection::Bookmarks,
            },
        ) {
            bookmarks.actions.push(spec);
        }
        for entry in self.bookmarks.entries().iter().take(MATERIALIZED_LIMIT) {
            let mut child = Resource::new(
                bookmark_resource_id(&entry.url),
                "browser.bookmark",
                entry.title.clone(),
            );
            if let Some(spec) = operation(
                "open",
                "Open bookmark",
                OperationKind::Effect,
                BrowserOperation::Navigate {
                    url: entry.url.clone(),
                },
            ) {
                child
                    .actions
                    .push(spec.with_contract(navigation_contract()));
            }
            if let Ok(spec) = ActionSpec::immediate(
                "remove",
                "Remove bookmark",
                BROWSER_ACTION_TYPE,
                &BrowserAction::RemoveBookmark {
                    url: entry.url.clone(),
                    observed_tags: crate::actions::observed_bookmark_tags(doc, &entry.url),
                },
            ) {
                child.actions.push(spec.destructive());
            }
            bookmarks.children.push(child);
        }

        let mut downloads = Resource::new("browser.downloads", "browser.downloads", "Downloads");
        if let Some(spec) = operation(
            "show",
            "Show downloads",
            OperationKind::View,
            BrowserOperation::ShowLibrary {
                section: LibrarySection::Downloads,
            },
        ) {
            downloads.actions.push(spec);
        }
        downloads.actions.push(
            ActionSpec::typed_operation(
                "start",
                "Download address",
                "https://…",
                OperationKind::Effect,
                BROWSER_OPERATION_TYPE,
                vec![ActionParam::required_owned(
                    "url",
                    ParamType::Text,
                    "Absolute HTTP or HTTPS URL",
                )],
                |args: &ActionArgs| {
                    encode_browser_operation(&BrowserOperation::StartDownload {
                        url: args.text("url").unwrap_or_default().to_string(),
                    })
                },
            )
            .with_contract(local_background_contract(&[
                "device-local",
                "network",
                "filesystem",
                "download",
            ])),
        );
        for entry in self
            .downloads
            .entries()
            .iter()
            .rev()
            .take(MATERIALIZED_LIMIT)
        {
            let mut child = Resource::new(
                entry.resource_id(),
                "browser.download",
                entry.display_name(),
            );
            if let Some(spec) = operation(
                "show",
                "Show download",
                OperationKind::View,
                BrowserOperation::ShowLibrary {
                    section: LibrarySection::Downloads,
                },
            ) {
                child.actions.push(spec);
            }
            match entry.status {
                DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => {
                    if let Some(spec) = operation(
                        "cancel",
                        "Cancel download",
                        OperationKind::Effect,
                        BrowserOperation::CancelDownload {
                            id: entry.id.clone(),
                        },
                    ) {
                        child.actions.push(
                            spec.with_contract(local_effect_contract(&[
                                "device-local",
                                "network",
                                "download",
                            ]))
                            .destructive(),
                        );
                    }
                }
                DownloadStatus::Failed | DownloadStatus::Cancelled => {
                    if let Some(spec) = operation(
                        "retry",
                        "Retry download",
                        OperationKind::Effect,
                        BrowserOperation::RetryDownload {
                            id: entry.id.clone(),
                        },
                    ) {
                        child
                            .actions
                            .push(spec.with_contract(local_background_contract(&[
                                "device-local",
                                "network",
                                "filesystem",
                                "download",
                            ])));
                    }
                }
                DownloadStatus::Completed => {
                    for (id, label, browser_operation) in [
                        (
                            "open",
                            "Open downloaded file",
                            BrowserOperation::OpenDownload {
                                id: entry.id.clone(),
                            },
                        ),
                        (
                            "reveal",
                            "Show downloaded file in Files",
                            BrowserOperation::RevealDownload {
                                id: entry.id.clone(),
                            },
                        ),
                    ] {
                        if let Some(spec) =
                            operation(id, label, OperationKind::Effect, browser_operation)
                        {
                            child
                                .actions
                                .push(spec.with_contract(local_background_contract(&[
                                    "device-local",
                                    "filesystem",
                                    "portal",
                                ])));
                        }
                    }
                }
            }
            downloads.children.push(child);
        }
        let mut extensions =
            Resource::new("browser.extensions", "browser.extensions", "Extensions");
        if let Some(spec) = operation(
            "show",
            "Show extensions",
            OperationKind::View,
            BrowserOperation::ShowLibrary {
                section: LibrarySection::Extensions,
            },
        ) {
            extensions.actions.push(spec);
        }
        // Installed add-ons are durable local profile entities even while the
        // WebKit activation API is unavailable. Publish each bounded,
        // address-safe inventory item so the action directory, invocation
        // history and agents use the same identity as the extension manager.
        for extension in crate::extensions::list_installed() {
            let mut child = Resource::new(
                crate::extensions::resource_id(&extension.id),
                "browser.extension",
                extension.name,
            );
            if let Some(spec) = operation(
                "show",
                "Show extension details",
                OperationKind::View,
                BrowserOperation::ShowLibrary {
                    section: LibrarySection::Extensions,
                },
            ) {
                child.actions.push(spec);
            }
            extensions.children.push(child);
        }
        // Library is a first-class presentation resource rather than an
        // unaddressable collection of tabs.  This lets keyboard users,
        // assistive technology, global action search, and agents all use the
        // same section and filter operations as the visible surface.
        let mut library = Resource::new("browser.library", "browser.library", "Library");
        for (id, label, section) in [
            ("show-history", "Show history", LibrarySection::History),
            (
                "show-bookmarks",
                "Show bookmarks",
                LibrarySection::Bookmarks,
            ),
            (
                "show-downloads",
                "Show downloads",
                LibrarySection::Downloads,
            ),
            (
                "show-extensions",
                "Show extensions",
                LibrarySection::Extensions,
            ),
        ] {
            if let Some(spec) = operation(
                id,
                label,
                OperationKind::View,
                BrowserOperation::ShowLibrary { section },
            ) {
                library.actions.push(spec);
            }
        }
        library.actions.push(
            ActionSpec::typed_operation(
                "set-filter",
                "Filter library",
                "keyword",
                OperationKind::View,
                BROWSER_OPERATION_TYPE,
                vec![ActionParam::required_owned(
                    "query",
                    ParamType::Text,
                    "Words to match in the current library section",
                )],
                |args: &ActionArgs| {
                    encode_browser_operation(&BrowserOperation::SetLibraryFilter {
                        query: args.text("query").unwrap_or_default().to_string(),
                    })
                },
            )
            .with_contract(local_effect_contract(&[
                "device-local",
                "presentation",
                "library",
            ])),
        );
        if let Some(spec) = operation(
            "clear-filter",
            "Clear library filter",
            OperationKind::View,
            BrowserOperation::ClearLibraryFilter,
        ) {
            library
                .actions
                .push(if self.library_filter.get().trim().is_empty() {
                    spec.unavailable("already_clear", "Library filter is already clear")
                } else {
                    spec
                });
        }
        let mut settings = Resource::new("browser.settings", "browser.settings", "Settings");
        if let Some(spec) = operation(
            "show",
            "Show browser settings",
            OperationKind::View,
            BrowserOperation::ShowSettings,
        ) {
            settings.actions.push(spec);
        }
        if let Some(spec) = operation(
            "request-site-data-clear",
            "Request local site-data clear",
            OperationKind::Effect,
            BrowserOperation::RequestWebsiteDataClear,
        ) {
            settings.actions.push(
                spec.with_contract(local_effect_contract(&[
                    "device-local",
                    "site-data",
                    "sensitive-data",
                ]))
                .destructive(),
            );
        }
        for (id, label, operation) in [
            ("set-adblock", "Set ad and tracker blocking", "set-adblock"),
            (
                "set-clear-on-exit",
                "Set clear data on exit",
                "set-clear-on-exit",
            ),
        ] {
            settings.actions.push(
                ActionSpec::typed(
                    id,
                    label,
                    "enabled",
                    ui::tilo_actions::ExecutionLocus::ClientLocal,
                    vec![ActionParam::required_owned(
                        "enabled",
                        ParamType::Boolean,
                        "Whether the setting is enabled",
                    )],
                    move |args: &ActionArgs| {
                        let action = if operation == "set-adblock" {
                            BrowserAction::SetAdblock {
                                enabled: args.boolean("enabled").unwrap_or(false),
                            }
                        } else {
                            BrowserAction::SetClearOnExit {
                                enabled: args.boolean("enabled").unwrap_or(false),
                            }
                        };
                        EncodedAction::encode(BROWSER_ACTION_TYPE, &action).unwrap_or_else(|_| {
                            EncodedAction {
                                type_id: BROWSER_ACTION_TYPE.into(),
                                payload: Vec::new(),
                            }
                        })
                    },
                )
                .reversible(),
            );
        }
        for (id, label, parameter, operation) in [
            ("set-homepage", "Set homepage", "url", "homepage"),
            (
                "set-search-engine",
                "Set search engine template",
                "template",
                "search-engine",
            ),
        ] {
            settings.actions.push(
                ActionSpec::typed(
                    id,
                    label,
                    parameter,
                    ui::tilo_actions::ExecutionLocus::ClientLocal,
                    vec![ActionParam::required_owned(
                        parameter,
                        ParamType::Text,
                        parameter,
                    )],
                    move |args: &ActionArgs| {
                        let action = if operation == "homepage" {
                            BrowserAction::SetHomepage {
                                url: args.text(parameter).unwrap_or_default().into(),
                            }
                        } else {
                            BrowserAction::SetSearchEngine {
                                template: args.text(parameter).unwrap_or_default().into(),
                            }
                        };
                        EncodedAction::encode(BROWSER_ACTION_TYPE, &action).unwrap_or_else(|_| {
                            EncodedAction {
                                type_id: BROWSER_ACTION_TYPE.into(),
                                payload: Vec::new(),
                            }
                        })
                    },
                )
                .reversible(),
            );
        }

        let mut browser = Resource::new("browser", "browser.application", "Browser");
        if let Some(spec) = operation(
            "show-page",
            "Show current page",
            OperationKind::View,
            BrowserOperation::ShowPage,
        ) {
            browser.actions.push(spec);
        }
        browser.children = vec![
            page, session, history, bookmarks, downloads, extensions, library, settings,
        ];
        vec![browser]
    }

    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == OperationKind::Document || type_id != BROWSER_OPERATION_TYPE {
            return Ok(false);
        }
        let operation = decode_browser_operation(payload)?;
        self.perform_browser_operation(operation)?;
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind == OperationKind::Document || type_id != BROWSER_OPERATION_TYPE {
            return Ok(None);
        }
        let operation = decode_browser_operation(payload)?;
        let is_navigation = matches!(
            &operation,
            BrowserOperation::Back
                | BrowserOperation::Forward
                | BrowserOperation::Reload
                | BrowserOperation::Navigate { .. }
        );
        if matches!(&operation, BrowserOperation::TogglePictureInPicture) {
            if self.resource_pip_invocation.is_some() || self.resource_pip_result.is_some() {
                return Err("another Picture in Picture operation is already running".into());
            }
            if self.active_pip {
                self.begin_pip_probe();
                return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                    local_output("Picture in Picture closed."),
                )));
            }
            self.begin_pip_probe();
            let request_id = self
                .active_pip_probe
                .ok_or_else(|| "Picture in Picture probe did not start".to_string())?;
            self.resource_pip_invocation = Some(invocation);
            self.resource_pip_request = Some(request_id);
            self.resource_pip_deadline = Some(std::time::Instant::now() + PIP_OPERATION_TIMEOUT);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !is_navigation {
            let is_history_pause_toggle =
                matches!(&operation, BrowserOperation::ToggleHistoryPause);
            let output = match &operation {
                BrowserOperation::ClearHistory => local_output("Browsing history cleared."),
                // WebKit exposes no completion event for this command. The
                // result deliberately says only what the executor can prove:
                // the request was accepted by the local engine.
                BrowserOperation::RequestWebsiteDataClear => {
                    local_output("Website-data clear request sent to the local browser engine.")
                }
                BrowserOperation::StartDownload { .. } => {
                    local_output("Download request sent to the local browser engine.")
                }
                BrowserOperation::CancelDownload { .. } => {
                    local_output("Download cancellation sent to the local browser engine.")
                }
                BrowserOperation::RetryDownload { .. } => {
                    local_output("Download retry sent to the local browser engine.")
                }
                BrowserOperation::OpenDownload { .. } => {
                    local_output("Downloaded file was handed to the desktop open portal.")
                }
                BrowserOperation::RevealDownload { .. } => {
                    local_output("Downloaded file location was handed to the desktop portal.")
                }
                _ => ui::tilo_actions::ActionOutput::default(),
            };
            self.perform_browser_operation(operation)?;
            let output = if is_history_pause_toggle {
                local_output(if self.history_paused {
                    "History paused in this window."
                } else {
                    "History resumed in this window."
                })
            } else {
                output
            };
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(output)));
        }
        if self.resource_navigation_invocation.is_some()
            || self.resource_navigation_result.is_some()
        {
            return Err("another browser navigation is already running".into());
        }
        self.perform_browser_operation(operation)?;
        self.resource_navigation_invocation = Some(invocation);
        self.resource_navigation_deadline = Some(std::time::Instant::now() + NAVIGATION_TIMEOUT);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        if self
            .resource_navigation_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.resource_navigation_result.take();
        }
        if self
            .resource_pip_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.resource_pip_result.take();
        }
        if self.resource_pip_invocation == Some(invocation)
            && self
                .resource_pip_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.resource_pip_invocation = None;
            self.resource_pip_request = None;
            self.resource_pip_deadline = None;
            return Some(ui::tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: ui::tilo_actions::ActionFailure {
                    code: "browser.picture_in_picture_timed_out".into(),
                    message: "Picture in Picture did not receive a media response in time.".into(),
                    details: Some(serde_json::json!({
                        "timeout_seconds": PIP_OPERATION_TIMEOUT.as_secs(),
                    })),
                    retryable: true,
                },
            });
        }
        if self.resource_navigation_invocation == Some(invocation)
            && self
                .resource_navigation_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.resource_navigation_invocation = None;
            self.resource_navigation_deadline = None;
            return Some(ui::tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: ui::tilo_actions::ActionFailure {
                    code: "browser.navigation_timed_out".into(),
                    message: "The page did not finish loading in time.".into(),
                    details: Some(serde_json::json!({
                        "timeout_seconds": NAVIGATION_TIMEOUT.as_secs(),
                    })),
                    retryable: true,
                },
            });
        }
        None
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Browser's frame-level input routing remains ordered in one place."
    )]
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.bootstrap_done {
            if !self.headless {
                // Privacy: "clear on exit" is honoured by clearing the
                // previous session's data at startup — reliable, unlike a
                // teardown hook the windowing layer doesn't expose.
                if self.settings.clear_on_exit {
                    self.engine.send(WebViewCommand::ClearWebsiteData);
                    self.history.clear();
                    let _ = self.history.save();
                }
                if self.settings.adblock_enabled {
                    crate::blocker::spawn_filter_loader(self.engine.clone());
                    // Scriptlet-style blocking (YouTube ad-skip etc.) the
                    // declarative content filter can't express.
                    self.engine
                        .send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
                            crate::blocker::user_scripts(),
                        )));
                }
                self.engine
                    .send(WebViewCommand::Navigate(self.initial_url.clone()));
            }
            self.bootstrap_done = true;
        }

        self.apply_engine_events();
        self.sync_settings_from_document(state, path);
        self.sync_bookmarks(state, path);
        self.sync_search_and_suggestions();

        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
        self.poll_active_pip_element();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme);
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child_node` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let compact = self.rect.width() < 600.0;
        if compact != self.compact_chrome {
            self.compact_chrome = compact;
            self.structure_dirty = true;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child_node) = children.first()
        {
            c.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
        // Anchor the suggestion dropdown flush under the address input.
        if let Some(r) = self.address_input_rect() {
            self.suggestions
                .set_anchor(Point::new(r.x0, r.y1 + 2.0), r.width() as f32);
        }
        self.publish_content_rect();
        self.refresh_active_pip_region();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// The suggestion dropdown is a field, not a tree child, so paint it
    /// in the overlay pass here (above the page) after the composed tree's
    /// own overlays.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // Re-anchor under the address bar every draw (layout may not have
        // re-run since the suggestions changed).
        if let Some(r) = self.address_input_rect() {
            self.suggestions
                .set_anchor(Point::new(r.x0, r.y1 + 2.0), r.width() as f32);
        }
        self.suggestions
            .draw_overlay(scene, transform, text_renderer, theme, presence);

        let feedback_hovered = self.pip_feedback_rect.contains(self.cursor);
        let float_page_hovered = self.pip_float_page_rect.contains(self.cursor);
        let select_area_hovered = self.pip_select_area_rect.contains(self.cursor);
        if !feedback_hovered
            && self.pip_feedback.as_ref().is_some_and(|feedback| {
                feedback.kind == PipFeedbackKind::NoVideo && feedback.is_expired()
            })
        {
            self.pip_feedback = None;
            self.pip_feedback_rect = Rect::ZERO;
        }
        self.pip_feedback_rect = Rect::ZERO;
        self.pip_float_page_rect = Rect::ZERO;
        self.pip_select_area_rect = Rect::ZERO;
        if let Some(feedback) = &self.pip_feedback {
            let elapsed = feedback.started.elapsed();
            let mut alpha = if self.motion_reduced {
                1.0
            } else {
                (elapsed.as_secs_f32() / PIP_FEEDBACK_FADE.as_secs_f32()).min(1.0)
            };
            if feedback.kind == PipFeedbackKind::NoVideo
                && !feedback.persistent
                && !feedback_hovered
            {
                let remaining = PIP_FEEDBACK_LIFETIME.saturating_sub(elapsed);
                alpha =
                    alpha.min((remaining.as_secs_f32() / PIP_FEEDBACK_FADE.as_secs_f32()).min(1.0));
            }
            if let Some(anchor) = self.pip_feedback_anchor() {
                let has_actions = feedback.kind == PipFeedbackKind::NoVideo;
                let Some(rect) = Self::pip_feedback_rect_for_anchor(self.rect, anchor, has_actions)
                else {
                    return;
                };
                self.pip_feedback_rect = rect;
                let card = match feedback.kind {
                    PipFeedbackKind::Finding => {
                        ToastCard::new("Finding the active video…", ToastKind::Info)
                            .caption("Picture in Picture")
                            .alpha(alpha)
                    }
                    PipFeedbackKind::NoVideo => {
                        ToastCard::new("No visible video on this page", ToastKind::Warning)
                            .caption("Picture in Picture")
                            .actions(
                                "Float page",
                                Icon::PictureInPicture2,
                                float_page_hovered,
                                "Select area",
                                Icon::MousePointer2,
                                select_area_hovered,
                            )
                            .alpha(alpha)
                    }
                };
                let hit = paint_card(scene, transform, text_renderer, theme, rect, &card);
                self.pip_float_page_rect = hit.secondary_action_rect;
                self.pip_select_area_rect = hit.primary_action_rect;
            }
        }
    }

    fn needs_animation_frame(&self) -> bool {
        if self.motion_reduced {
            return self
                .composed
                .as_ref()
                .is_some_and(|composed| composed.needs_animation_frame());
        }
        let feedback_animates = self.pip_feedback.as_ref().is_some_and(|feedback| {
            feedback.started.elapsed() < PIP_FEEDBACK_FADE
                || (feedback.kind == PipFeedbackKind::NoVideo
                    && !feedback.persistent
                    && feedback.started.elapsed() < PIP_FEEDBACK_LIFETIME)
        });
        feedback_animates
            || self
                .composed
                .as_ref()
                .is_some_and(|composed| composed.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Toggling the settings pane swaps the content widget → rebuild.
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|c| c.take_structure_dirty())
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if let Some(composed) = &mut self.composed {
            composed.set_motion_reduced(reduced);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) && self
            .pip_feedback
            .as_ref()
            .is_some_and(|feedback| feedback.kind == PipFeedbackKind::NoVideo)
        {
            if self.pip_select_area_rect.contains(self.cursor) {
                self.pending_capture_studio = Some(ui::widget::CaptureMode::Pip);
                self.pip_feedback = None;
                self.pip_feedback_rect = Rect::ZERO;
                self.pip_float_page_rect = Rect::ZERO;
                self.pip_select_area_rect = Rect::ZERO;
                *handled = true;
                return;
            }
            if self.pip_float_page_rect.contains(self.cursor) {
                self.float_current_page();
                *handled = true;
                return;
            }
        }

        if self.handle_address_shortcut(event, *modifiers) {
            *handled = true;
            return;
        }

        // Ctrl/Cmd+F is scoped to Library; normal page find remains the
        // website's concern. The visible filter has a canonical resource
        // operation too, so this is only a convenient focus path.
        if self.handle_library_shortcut(event, *modifiers) {
            *handled = true;
            return;
        }

        // Conventional browser navigation keys. These must be handled before
        // forwarding into the page: an embedded site should not be able to
        // steal the browser's own history controls. The queued invocation is
        // the same canonical action the toolbar exposes to history, agents,
        // and collaboration.
        if self.handle_browser_shortcut(event, *modifiers) {
            *handled = true;
            return;
        }

        // The suggestion dropdown gets first crack at input while it's up
        // (address bar focused): arrow keys move the selection, Enter/click
        // pick a suggestion, Escape dismisses — ahead of the text field.
        if self.suggestions.is_visible() && !self.show_settings {
            let mut ov_handled = false;
            self.suggestions.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut ov_handled,
                focus_target,
                node_id,
                taffy,
            );
            if let Some(entry) = self.suggestions.take_chosen() {
                self.suggestions.clear();
                self.last_query = None;
                self.address.set(entry.url.clone());
                self.queue_navigate(entry.url);
                *handled = true;
                return;
            }
            if ov_handled {
                *handled = true;
                return;
            }
        }

        let filter_before = self.library_filter.get();
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child_node) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
        let filter_after = self.library_filter.get();
        if self.show_library && filter_after != filter_before {
            let bounded = bounded_library_filter(&filter_after);
            if bounded != filter_after {
                self.library_filter.set(bounded);
            }
            // `TextInput` owns the immediate edit, but the result rows are
            // rebuilt by Browser. Mark this immediately so typing never
            // leaves stale rows under a changed query for a frame.
            self.structure_dirty = true;
        }

        // Enter in the focused address bar submits it (no suggestion picked).
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && key.logical_key == WKey::Named(WNamedKey::Enter)
            && self.url_input_focused()
        {
            self.navigate_to_input();
            self.suggestions.clear();
            self.last_query = None;
            *handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_controls.pop_front().or_else(|| {
            self.composed
                .as_mut()
                .and_then(|composed| composed.take_control_invocation())
        })
    }

    fn name(&self) -> &'static str {
        "BrowserWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_id(&self) -> Option<&str> {
        Some("browser.root")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self
            .pip_feedback
            .as_ref()
            .is_some_and(|feedback| feedback.kind == PipFeedbackKind::NoVideo)
            && (self.pip_float_page_rect.contains(point)
                || self.pip_select_area_rect.contains(point)))
        .then_some(winit::window::CursorIcon::Pointer)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        // The suggestion list paints in Browser's overlay pass instead of
        // being a retained layout child. Publish it from the Browser owner so
        // it is present in the same semantic tree as the address field and
        // its virtual rows resolve actions back to this widget.
        if !self.suggestions.is_visible() {
            return None;
        }
        self.suggestions.accessibility().map(|suggestions| {
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Browser")
                .with_children(vec![suggestions])
        })
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if !self
            .suggestions
            .perform_accessibility_action(target, action)
        {
            return false;
        }
        if let Some(entry) = self.suggestions.take_chosen() {
            self.suggestions.clear();
            self.last_query = None;
            self.address.set(entry.url.clone());
            // This queues Browser's existing typed `browser.page/navigate`
            // action — AT-SPI does not get a parallel imperative navigation
            // route.
            self.queue_navigate(entry.url);
        }
        true
    }

    fn modal_capture(&self) -> Option<Rect> {
        // Overlay widgets are not ordinary retained children, so the trait's
        // default child walk cannot see their capture rect. Without this,
        // a click on the suggestion panel could leak through to the page.
        self.suggestions.modal_capture().or_else(|| {
            self.composed
                .as_ref()
                .and_then(|composed| composed.modal_capture())
        })
    }
}

impl AppWidget for BrowserWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        if self.title.is_empty() {
            "Browser"
        } else {
            &self.title
        }
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{F02AC}") // mdi-web (browser globe glyph)
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}

#[cfg(test)]
mod architecture_tests {
    use super::*;

    fn compose(widget: &mut BrowserWidget) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&mut doc, &root);
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
    }

    fn assert_no_document_control_paths(widget: &BrowserWidget) {
        assert!(widget.control_action_bindings().iter().all(|binding| {
            !matches!(
                binding.kind,
                ui::widget::ControlBindingKind::CollaborativeDocument(_)
            )
        }));
    }

    fn assert_action_binding(
        widget: &BrowserWidget,
        control_id: &str,
        resource: &str,
        action: &str,
    ) {
        assert!(
            widget.control_action_bindings().iter().any(|binding| {
                binding.control_id == control_id
                    && matches!(
                        &binding.kind,
                        ui::widget::ControlBindingKind::Action(address)
                            if address.resource == resource && address.action == action
                    )
            }),
            "{control_id} must retain its canonical {resource}/{action} address"
        );
    }

    #[test]
    fn presentation_operations_are_peer_local_and_do_not_mutate_the_document() {
        let mut left = BrowserWidget::new_headless();
        let right = BrowserWidget::new_headless();
        let mut doc = AutoCommit::new();
        let before = doc.save();
        left.perform_browser_operation(BrowserOperation::ShowLibrary {
            section: LibrarySection::Bookmarks,
        })
        .unwrap();
        assert!(left.show_library);
        assert!(!right.show_library);
        assert_eq!(
            doc.save(),
            before,
            "showing a local pane is not a shared mutation"
        );
    }

    #[test]
    fn library_open_controls_keep_stable_resource_addresses_after_reorder() {
        let mut widget = BrowserWidget::new_headless();
        widget
            .history
            .record_at("https://first.example", "First", 1);
        widget
            .history
            .record_at("https://second.example", "Second", 2);
        widget.show_library = true;
        compose(&mut widget);
        let bindings = widget.control_action_bindings();
        for url in ["https://first.example", "https://second.example"] {
            let resource = history_resource_id(url);
            assert!(bindings.iter().any(|binding| {
                binding.control_id == format!("{resource}.open")
                    && matches!(
                        &binding.kind,
                        ui::widget::ControlBindingKind::Action(address)
                            if address.resource == resource && address.action == "open"
                    )
            }));
        }
    }

    #[test]
    fn library_filter_is_a_local_canonical_action_with_a_bound_control() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("bookmarks");
        compose(&mut widget);
        assert_action_binding(
            &widget,
            "browser.library.clear-filter",
            "browser.library",
            "clear-filter",
        );

        widget
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "rust learn".into(),
            })
            .unwrap();
        assert_eq!(widget.library_filter.get(), "rust learn");
        assert!(widget.show_library);
        assert!(widget.take_structure_dirty());
        assert!(
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "x".repeat(MAX_LIBRARY_FILTER_BYTES + 1),
                })
                .is_err()
        );
    }

    #[test]
    fn every_hosted_browser_affordance_uses_a_canonical_action() {
        let mut page = BrowserWidget::new_headless();
        compose(&mut page);
        assert_no_document_control_paths(&page);
        for (control, resource, action) in [
            ("browser.back", "browser.page", "back"),
            ("browser.forward", "browser.page", "forward"),
            ("browser.reload", "browser.page", "reload"),
            ("browser.bookmark", "browser.page", "bookmark"),
            (
                "browser.history-pause",
                "browser.session",
                "toggle-history-pause",
            ),
            ("browser.library", "browser.history", "show"),
            ("browser.pip", "browser.page", "picture-in-picture"),
            ("browser.settings", "browser.settings", "show"),
        ] {
            assert_action_binding(&page, control, resource, action);
        }

        let mut library = BrowserWidget::new_headless();
        library.debug_show_library("history");
        compose(&mut library);
        assert_no_document_control_paths(&library);
        assert_action_binding(
            &library,
            "browser.library.clear-filter",
            "browser.library",
            "clear-filter",
        );

        let mut settings = BrowserWidget::new_headless();
        settings.show_settings = true;
        compose(&mut settings);
        assert_no_document_control_paths(&settings);
        for (control, resource, action) in [
            ("browser.settings", "browser", "show-page"),
            ("settings.close", "browser", "show-page"),
            ("settings.adblock", "browser.settings", "set-adblock"),
            (
                "settings.clear_on_exit",
                "browser.settings",
                "set-clear-on-exit",
            ),
            ("settings.clear_history", "browser.history", "clear"),
            (
                "settings.clear_cookies",
                "browser.settings",
                "request-site-data-clear",
            ),
        ] {
            assert_action_binding(&settings, control, resource, action);
        }
    }
}

#[cfg(test)]
#[allow(
    unused_mut,
    unused_variables,
    reason = "legacy layout helpers retain state handles for parity with live setup"
)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ui::webview::WebViewEngine;

    fn synced(widget: &mut BrowserWidget) -> (AutoCommit, ObjId) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&mut state, &root);
        (state, root)
    }

    fn history_save_fails(_: &History) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test history directory is not writable",
        ))
    }

    fn layout_at(widget: &mut BrowserWidget, width: f32, height: f32) {
        layout_with_theme(widget, width, height, &Theme::dark());
    }

    fn layout_with_theme(widget: &mut BrowserWidget, width: f32, height: f32, theme: &Theme) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&mut state, &root);
        let child = widget.build_layout(&mut tree, theme, &mut nodes);
        widget.sync(&mut state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(width),
                        height: Dimension::length(height),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_nodes = nodes.clone();
        tree.compute_layout_with_measure(
            layout_root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, child, Point::ORIGIN);
    }

    #[test]
    fn nav_gesture_drives_engine_history() {
        use ui::gesture::NavDirection;
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);
        // Build the composed tree so the gesture forwards down to the
        // WebView — this exercises the real trait-forwarding path
        // (BrowserWidget → Column → WebView), not a shortcut.
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        w.build_layout(&mut tree, &theme, &mut node_map);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        assert!(
            w.on_nav_gesture(NavDirection::Back, &mut state, &root),
            "a back gesture is consumed by the embedded WebView",
        );
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        w.on_nav_gesture(NavDirection::Forward, &mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
    }

    #[test]
    fn bootstrap_seeds_address_bar() {
        let engine = TestEngine::new();
        // Headless so the test never spawns the network filter loader.
        let mut w = BrowserWidget::with_engine(engine, true);
        let (state, root) = synced(&mut w);
        assert_eq!(w.address.with(Clone::clone), DEFAULT_URL);
    }

    #[test]
    fn address_shortcut_focuses_the_chrome_input_after_composition() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let _ = synced(&mut w);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

        assert!(w.focus_address_input());
        assert!(w.url_input_focused());
    }

    #[test]
    fn library_filter_matches_every_word_across_titles_and_addresses() {
        assert!(library_filter_matches(
            "rust learn",
            &["Learn Rust", "https://www.rust-lang.org/learn"],
        ));
        assert!(library_filter_matches(
            "RUST",
            &["Learn Rust", "https://www.rust-lang.org/learn"],
        ));
        assert!(!library_filter_matches(
            "rust missing",
            &["Learn Rust", "https://www.rust-lang.org/learn"],
        ));
    }

    #[test]
    fn library_filter_bound_preserves_utf8_boundaries() {
        let query = format!("rust {}", "🦀".repeat(MAX_LIBRARY_FILTER_BYTES));
        let bounded = bounded_library_filter(&query);
        assert!(bounded.len() <= MAX_LIBRARY_FILTER_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn library_filter_is_visible_and_keyboard_focusable_at_compact_large_text() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("bookmarks");
        let theme = Theme::dark().with_text_scale(2.0);
        layout_with_theme(&mut widget, 390.0, 780.0, &theme);
        assert!(widget.find_rect(LIBRARY_FILTER_INPUT_ID).is_some());
        assert!(widget.focus_input(LIBRARY_FILTER_INPUT_ID));
    }

    #[test]
    fn address_validation_never_dispatches_non_web_inputs_and_keeps_a_recoverable_error() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (_, _) = synced(&mut w);
        let commands_before = engine.commands().len();
        let overlong = "x".repeat(8 * 1024 + 1);

        for input in [
            "",
            overlong.as_str(),
            "https://",
            "https://bad host",
            "file:///tmp/private.html",
            "about:blank",
            "data:text/html,hello",
            "javascript:alert(1)",
            "custom:opaque",
            "https://example.test/\u{0007}",
        ] {
            let error = w.navigation_target(input).expect_err(input);
            assert!(!error.is_empty());
            assert_eq!(w.load_error.as_deref(), Some(error.as_str()));
            assert!(w.address_error_active);
            assert_eq!(engine.commands().len(), commands_before, "{input}");
        }
    }

    #[test]
    fn address_validation_accepts_local_web_hosts_but_not_local_files() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        for (input, expected) in [
            ("localhost:3000", "https://localhost:3000"),
            ("127.0.0.1:8080", "https://127.0.0.1:8080"),
            ("[::1]:8443", "https://[::1]:8443"),
        ] {
            assert_eq!(w.navigation_target(input).unwrap(), expected);
        }
        assert!(w.navigation_target("file:///tmp/index.html").is_err());
    }

    #[test]
    fn stale_engine_completion_cannot_clear_a_rejected_address_error() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // Simulate events from an earlier navigation arriving after the user
        // entered a prohibited address. The error must remain actionable,
        // rather than disappearing behind that stale successful page.
        engine.send(WebViewCommand::Navigate("https://earlier.example".into()));
        assert!(w.navigation_target("file:///tmp/not-allowed.html").is_err());
        w.apply_engine_events();
        assert!(w.address_error_active);
        assert_eq!(
            w.load_error.as_deref(),
            Some("Browser can open only http:// or https:// addresses.")
        );

        assert!(
            w.perform_browser_operation(BrowserOperation::Navigate {
                url: "https://recovery.example".into(),
            })
            .is_ok()
        );
        assert!(
            !w.address_error_active,
            "an allowed retry recovers the page"
        );
    }

    #[test]
    fn narrow_chrome_keeps_a_nonzero_page_viewport() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(390);
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&mut state, &root);
        let child = widget.build_layout(&mut tree, &theme, &mut nodes);
        widget.sync(&mut state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(390.0),
                        height: Dimension::length(844.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_nodes = nodes.clone();
        tree.compute_layout_with_measure(
            layout_root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(390.0),
                height: taffy::AvailableSpace::Definite(844.0),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, child, Point::ORIGIN);
        let viewport = widget.find_rect("browser.surface").unwrap();
        let address = widget.find_rect(URL_INPUT_ID).unwrap();
        assert!(viewport.width() > 0.0 && viewport.height() > 0.0);
        assert!(viewport.y0 < 100.0, "viewport starts beneath the toolbar");
        assert!(
            address.width() >= 140.0,
            "compact chrome preserves a usable address/search field"
        );
    }

    #[test]
    fn compact_history_library_retains_the_hidden_toolbar_privacy_action() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("history");
        widget.debug_set_viewport_width(390);
        layout_at(&mut widget, 390.0, 844.0);
        assert!(widget.control_action_bindings().iter().any(|binding| {
            binding.control_id == "browser.library.history-pause"
                && matches!(
                    &binding.kind,
                    ui::widget::ControlBindingKind::Action(address)
                        if address.resource == "browser.session"
                            && address.action == "toggle-history-pause"
                )
        }));
    }

    #[test]
    fn address_suggestions_are_modal_and_a11y_actions_queue_canonical_navigation() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_force_suggestions(vec![crate::history::HistoryEntry {
            url: "https://accessible.example/reading".into(),
            title: "Accessible reading".into(),
            last_visit: 1,
            visit_count: 1,
        }]);
        layout_at(&mut widget, 390.0, 844.0);

        let mut semantic = ui::a11y_tree::SemanticTree::new();
        semantic.refresh(&widget, &std::collections::HashMap::new(), None, &[]);
        let suggestion = semantic
            .iter()
            .find(|node| node.name.as_deref() == Some("Accessible reading"))
            .expect("floating address suggestions must reach the semantic tree");
        assert!(suggestion.action("activate").is_some());
        assert!(Widget::modal_capture(&widget).is_some());

        let target = format!(
            "browser.address-suggestion:{}",
            blake3::hash(b"https://accessible.example/reading").to_hex()
        );
        assert!(Widget::perform_semantic_action(
            &mut widget,
            &target,
            &ui::a11y::Action::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        let invocation = widget
            .take_control_invocation()
            .expect("accessibility activation must queue Browser's action binding");
        assert_eq!(invocation.address.resource, "browser.page");
        assert_eq!(invocation.address.action, "navigate");
        assert!(Widget::modal_capture(&widget).is_none());
    }

    #[test]
    fn populated_library_rows_have_a_bounded_compact_measure_at_all_widths() {
        let mut wide = BrowserWidget::new_headless();
        wide.debug_show_library("history");
        layout_at(&mut wide, 1_200.0, 720.0);
        let first = wide
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        let second = wide
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://developer.mozilla.org/en-US/docs/Web")
            ))
            .unwrap();
        let history_tab = wide.find_rect("browser.library.history").unwrap();
        assert!(
            (120.0..=160.0).contains(&history_tab.x0),
            "desktop title, tabs, and rows share a centered 920px Library measure"
        );
        assert!(
            first.x1 < 1_080.0,
            "desktop row actions stay at the trailing edge of the centered Library measure"
        );
        assert!(first.height() <= 40.0);
        assert!(
            second.y0 - first.y0 < 80.0,
            "two-line metadata rows must not expand into oversized slabs"
        );

        let mut narrow = BrowserWidget::new_headless();
        narrow.debug_show_library("bookmarks");
        narrow.debug_set_viewport_width(390);
        layout_at(&mut narrow, 390.0, 844.0);
        let open = narrow
            .find_rect(&format!(
                "{}.open",
                bookmark_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        let remove = narrow
            .find_rect(&format!(
                "{}.remove",
                bookmark_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        assert!(open.width() > 0.0 && remove.width() > 0.0);
        assert!(
            remove.x1 <= 390.0,
            "row actions stay reachable on narrow layouts"
        );
        let history_tab = narrow.find_rect("browser.library.history").unwrap();
        let bookmarks_tab = narrow.find_rect("browser.library.bookmarks").unwrap();
        let downloads_tab = narrow.find_rect("browser.library.downloads").unwrap();
        let extensions_tab = narrow.find_rect("browser.library.extensions").unwrap();
        assert!(
            history_tab.x1 <= 390.0
                && bookmarks_tab.x1 <= 390.0
                && downloads_tab.x1 <= 390.0
                && extensions_tab.x1 <= 390.0,
            "every Library destination stays visible instead of overflowing horizontally"
        );
        assert!(
            downloads_tab.y0 > history_tab.y0,
            "compact Library uses a deliberate two-row section grid"
        );
    }

    #[test]
    fn large_text_desktop_library_keeps_a_centered_reading_measure() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("history");
        widget.debug_set_viewport_width(1_200);
        layout_with_theme(
            &mut widget,
            1_200.0,
            800.0,
            &Theme::dark().with_text_scale(2.0),
        );

        let history_tab = widget.find_rect("browser.library.history").unwrap();
        let open = widget
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        assert!(
            (120.0..=160.0).contains(&history_tab.x0),
            "large text retains the desktop Library measure instead of becoming edge-to-edge"
        );
        assert!(
            open.x1 < 1_080.0,
            "large-text row actions stay inside the centered Library measure"
        );
    }

    #[test]
    fn toolbar_surface_controls_advertise_the_action_their_labels_promise() {
        fn address_for(widget: &BrowserWidget, control: &str) -> ui::tilo_actions::ActionAddress {
            widget
                .control_action_bindings()
                .into_iter()
                .find_map(|binding| match binding.kind {
                    ui::widget::ControlBindingKind::Action(address)
                        if binding.control_id == control =>
                    {
                        Some(address)
                    }
                    _ => None,
                })
                .unwrap()
        }

        let mut widget = BrowserWidget::new_headless();
        let mut doc = AutoCommit::new();
        widget.sync(&mut doc, &automerge::ROOT);
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(address_for(&widget, "browser.library").action, "show");
        assert_eq!(address_for(&widget, "browser.settings").action, "show");

        widget.show_library = true;
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(
            address_for(&widget, "browser.library").action,
            "show-page",
            "a control labelled Close browser library must close it"
        );

        widget.show_library = false;
        widget.show_settings = true;
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(
            address_for(&widget, "browser.settings").action,
            "show-page",
            "a control labelled Close browser settings must close it"
        );
        assert_eq!(address_for(&widget, "settings.close").action, "show-page");
    }

    #[test]
    fn loading_progress_keeps_toolbar_above_the_page_surface() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_loading();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&mut state, &root);
        let child = widget.build_layout(&mut tree, &theme, &mut nodes);
        widget.sync(&mut state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(900.0),
                        height: Dimension::length(620.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_nodes = nodes.clone();
        tree.compute_layout_with_measure(
            layout_root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(900.0),
                height: taffy::AvailableSpace::Definite(620.0),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, child, Point::ORIGIN);
        let toolbar = widget.find_rect("browser.reload").unwrap();
        let viewport = widget.find_rect("browser.surface").unwrap();
        assert!(toolbar.height() > 0.0);
        assert!(
            viewport.y0 > toolbar.y1,
            "loading must reserve a strip below the toolbar, not let WebView cover it"
        );
    }

    #[test]
    fn narrow_pip_feedback_is_clamped_inside_viewport_safe_bounds() {
        let viewport = Rect::new(0.0, 0.0, 390.0, 844.0);
        // Compact chrome has no dedicated PiP button; its semantic fallback
        // anchor is close enough to the trailing edge that trailing-only
        // alignment used to put the left side of this card offscreen.
        let anchor = Rect::new(278.0, 12.0, 322.0, 52.0);
        let card = BrowserWidget::pip_feedback_rect_for_anchor(viewport, anchor, true).unwrap();
        assert!(card.x0 >= viewport.x0 + 12.0);
        assert!(card.x1 <= viewport.x1 - 12.0);
        assert!(card.y0 >= viewport.y0 + 12.0);
        assert!(card.y1 <= viewport.y1 - 12.0);
        assert!(
            (card.width() - 354.0).abs() < f64::EPSILON,
            "the full action card still fits"
        );
    }

    #[test]
    fn compact_pip_feedback_uses_a_visible_toolbar_anchor() {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(390);
        widget.debug_show_empty_pip_feedback();
        layout_at(&mut widget, 390.0, 844.0);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        widget.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );

        assert!(widget.pip_feedback_rect.width() > 0.0);
        assert!(widget.pip_feedback_rect.x0 >= 12.0);
        assert!(widget.pip_feedback_rect.x1 <= 378.0);
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "compact feedback must remain visible even without a PiP toolbar button"
        );
    }

    #[test]
    fn canonical_navigation_operations_translate_to_engine_commands() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // History so back/forward are meaningful.
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        w.perform_browser_operation(BrowserOperation::Back).unwrap();
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        w.perform_browser_operation(BrowserOperation::Forward)
            .unwrap();
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));

        w.perform_browser_operation(BrowserOperation::Reload)
            .unwrap();
        assert!(
            engine.commands().contains(&WebViewCommand::Reload),
            "canonical reload reaches the engine",
        );
    }

    #[test]
    fn pip_button_probes_and_floats_only_the_video_rect() {
        let engine = TestEngine::new();
        engine.set_media_element(Some(MediaElementInfo {
            element_id: "media:main".into(),
            viewport_rect: [0.1, 0.2, 0.5, 0.4],
            intrinsic_size: [1920, 1080],
            label: "The active film".into(),
        }));
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);
        w.rect = Rect::new(100.0, 50.0, 900.0, 650.0);
        *w.content_rect.lock().unwrap() = Some([0.0, 0.1, 1.0, 1.0]);

        w.begin_pip_probe();
        w.sync(&mut state, &root); // accepts/sends the asynchronous probe
        assert!(
            engine
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::RequestMediaElement { .. }))
        );
        let request = w
            .take_floating_surface_request()
            .expect("a visible video becomes PiP");
        assert_eq!(request.title, "The active film");
        assert_eq!(request.aspect_ratio, Some(16.0 / 9.0));
        let ui::floating_surface::FloatingSource::AppElement {
            element_id, region, ..
        } = request.source
        else {
            panic!("browser PiP should preserve its semantic media identity");
        };
        assert_eq!(element_id, "media:main");
        let expected = Rect::new(180.0, 218.0, 580.0, 434.0);
        assert!((region.x0 - expected.x0).abs() < 0.001);
        assert!((region.y0 - expected.y0).abs() < 0.001);
        assert!((region.x1 - expected.x1).abs() < 0.001);
        assert!((region.y1 - expected.y1).abs() < 0.001);
        assert_eq!(w.active_floating_surface_ids(), vec![PIP_ID.to_string()]);

        w.on_floating_surface_host_message(
            &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: PIP_ID.into(),
                rect: [0.0, 0.0, 1.0, 1.0],
            },
        );
        w.rect = Rect::new(200.0, 80.0, 1_200.0, 780.0);
        w.refresh_active_pip_region();
        let updates = w.take_floating_surface_source_updates();
        assert_eq!(
            updates.len(),
            1,
            "a resized browser republishes the video box"
        );
        assert_ne!(updates[0].1, expected);

        engine.set_media_element(Some(MediaElementInfo {
            element_id: "media:main".into(),
            viewport_rect: [0.18, 0.12, 0.46, 0.36],
            intrinsic_size: [1920, 1080],
            label: "The active film".into(),
        }));
        w.next_pip_refresh_at = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(1))
                .unwrap_or_else(std::time::Instant::now),
        );
        w.poll_active_pip_element();
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::RequestMediaElementById { element_id, .. }
                if element_id == "media:main"
        )));
        w.apply_engine_events();
        let semantic_updates = w.take_floating_surface_source_updates();
        assert_eq!(semantic_updates.len(), 1);
        assert_ne!(semantic_updates[0].1, updates[0].1);

        w.begin_pip_probe();
        assert!(
            w.active_floating_surface_ids().is_empty(),
            "the toolbar button toggles an adopted PiP off instead of probing twice"
        );

        w.on_floating_surface_host_message(
            &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: PIP_ID.into() },
        );
        assert!(w.active_pip_element.is_none());
    }

    #[test]
    fn pip_without_video_explains_the_shell_selection_fallback() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine, true);
        let (mut state, root) = synced(&mut w);

        w.begin_pip_probe();
        w.sync(&mut state, &root);
        w.sync(&mut state, &root);

        assert!(w.take_floating_surface_request().is_none());
        assert_eq!(
            w.pip_feedback.as_ref().map(|feedback| feedback.kind),
            Some(PipFeedbackKind::NoVideo)
        );
    }

    #[test]
    fn reduced_motion_settles_pip_feedback_without_requesting_frames() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        w.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now(),
            persistent: false,
        });

        assert!(w.needs_animation_frame());
        w.set_motion_reduced(true);
        assert!(
            !w.needs_animation_frame(),
            "reduced motion keeps the feedback visible but does not animate its fade"
        );
    }

    #[test]
    fn no_video_select_area_requests_the_shared_shell_pip_picker_once() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let (mut state, root) = synced(&mut w);
        w.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now(),
            persistent: false,
        });
        w.pip_feedback_rect = Rect::new(100.0, 80.0, 454.0, 192.0);
        w.pip_float_page_rect = Rect::new(114.0, 154.0, 273.0, 184.0);
        w.pip_select_area_rect = Rect::new(281.0, 154.0, 440.0, 184.0);
        w.cursor = w.pip_select_area_rect.center();

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).expect("test leaf");
        let mut handled = false;
        let mut focus = None;
        w.handle_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );

        assert!(handled);
        assert_eq!(
            w.take_capture_studio_request(),
            Some(ui::widget::CaptureMode::Pip)
        );
        assert!(w.take_capture_studio_request().is_none());
        assert!(w.take_floating_surface_request().is_none());
        assert!(w.pip_feedback.is_none());
    }

    #[test]
    fn no_video_fallback_floats_and_tracks_the_page_viewport() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        w.rect = Rect::new(100.0, 50.0, 900.0, 650.0);
        *w.content_rect.lock().unwrap() = Some([0.0, 0.1, 1.0, 1.0]);
        w.title = "Reference board".into();

        w.float_current_page();
        let request = w
            .take_floating_surface_request()
            .expect("the actionable no-video card creates a real live mirror");
        assert_eq!(request.title, "Reference board");
        let ui::floating_surface::FloatingSource::AppRegion { region, .. } = request.source else {
            panic!("page fallback should track the page viewport, not invent a media element");
        };
        let expected = Rect::new(100.0, 110.0, 900.0, 650.0);
        assert!((region.x0 - expected.x0).abs() < 0.001);
        assert!((region.y0 - expected.y0).abs() < 0.001);
        assert!((region.x1 - expected.x1).abs() < 0.001);
        assert!((region.y1 - expected.y1).abs() < 0.001);
        assert!(w.active_pip && w.pip_page_fallback);

        w.rect = Rect::new(200.0, 80.0, 1_200.0, 780.0);
        w.refresh_active_pip_region();
        let updates = w.take_floating_surface_source_updates();
        assert_eq!(
            updates.len(),
            1,
            "a resized page keeps its live crop current"
        );
        assert_ne!(updates[0].1, region);

        w.poll_active_pip_element();
        assert!(
            !engine
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::RequestMediaElementById { .. })),
            "page fallback must not be closed by semantic-video polling"
        );
        w.on_floating_surface_host_message(
            &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: PIP_ID.into() },
        );
        assert!(!w.active_pip && !w.pip_page_fallback);
    }

    #[test]
    fn actionable_pip_feedback_does_not_expire_under_the_pointer() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        w.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now()
                .checked_sub(PIP_FEEDBACK_LIFETIME + std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
            persistent: false,
        });
        w.pip_feedback_rect = Rect::new(100.0, 80.0, 426.0, 144.0);
        w.cursor = w.pip_feedback_rect.center();
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        w.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(w.pip_feedback.is_some());

        w.cursor = Point::ZERO;
        w.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(w.pip_feedback.is_none());
    }

    #[test]
    fn active_pip_closes_when_its_semantic_element_disappears() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let (mut state, root) = synced(&mut w);
        w.active_pip = true;
        w.active_pip_element = Some(MediaElementInfo {
            element_id: "media:gone".into(),
            viewport_rect: [0.1, 0.1, 0.8, 0.6],
            intrinsic_size: [1_280, 720],
            label: "Temporary video".into(),
        });
        w.next_pip_refresh_at = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(1))
                .unwrap_or_else(std::time::Instant::now),
        );

        w.poll_active_pip_element();
        w.apply_engine_events();

        assert!(!w.active_pip);
        assert!(w.active_pip_element.is_none());
        assert!(w.active_floating_surface_ids().is_empty());
        assert!(w.pip_feedback.is_some());
    }

    #[test]
    fn engine_uri_updates_address_bar_text() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://example.test".into()));
        w.sync(&mut state, &root);

        assert_eq!(w.address.with(Clone::clone), "https://example.test");
        assert_eq!(w.app_title(), "https://example.test — test page");
    }

    #[test]
    fn history_events_drive_button_enablement() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        w.sync(&mut state, &root);

        assert!(w.can_go_back);
        assert!(!w.can_go_forward);
    }

    #[test]
    fn headless_engine_has_deterministic_page() {
        let w = BrowserWidget::new_headless();
        let slot = w.engine.frames();
        let frame = slot.lock().unwrap().clone().expect("pre-rendered frame");
        assert!(frame.width > 0 && frame.height > 0);
    }

    #[test]
    fn default_engine_is_shareable() {
        // The engine handle must be cloneable/shareable between widget,
        // app, and blocker thread.
        let engine: SharedEngine = default_engine();
        let _clone: SharedEngine = Arc::clone(&engine);
    }

    #[test]
    fn resource_surface_binds_typed_navigation_and_reports_history_availability() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut w);
        let resources = Widget::resources(&w, &doc);
        assert_eq!(resources[0].id.0, "browser");
        for expected in [
            "browser.page",
            "browser.history",
            "browser.bookmarks",
            "browser.downloads",
            "browser.extensions",
            "browser.library",
            "browser.settings",
        ] {
            assert!(
                resources[0]
                    .children
                    .iter()
                    .any(|child| child.id.0 == expected)
            );
        }
        let page = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.page")
            .unwrap();
        let bookmark = page.actions.iter().find(|a| a.id == "bookmark").unwrap();
        assert_eq!(bookmark.kind, OperationKind::Document);
        let back = page.actions.iter().find(|a| a.id == "back").unwrap();
        assert!(!back.contract.availability.is_available());
        let navigate = page.actions.iter().find(|a| a.id == "navigate").unwrap();
        assert_eq!(navigate.params()[0].name.as_ref(), "url");
        assert_eq!(
            navigate.contract.execution,
            ui::tilo_actions::ExecutionMode::Background { cancellable: false }
        );
        assert!(navigate.contract.validate_declaration().is_empty());
        assert!(
            navigate
                .contract
                .result
                .validate(&ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::Value::String("https://example.test".into())),
                    resources: Vec::new(),
                })
                .is_ok()
        );
        let settings = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.settings")
            .unwrap();
        let library = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.library")
            .unwrap();
        let filter = library
            .actions
            .iter()
            .find(|action| action.id == "set-filter")
            .unwrap();
        assert_eq!(filter.kind, OperationKind::View);
        assert_eq!(filter.params()[0].name.as_ref(), "query");
        assert!(filter.contract.validate_declaration().is_empty());
        assert_eq!(
            settings
                .actions
                .iter()
                .find(|a| a.id == "set-adblock")
                .unwrap()
                .kind,
            OperationKind::Document
        );
        let mut args = serde_json::Map::new();
        args.insert("url".into(), serde_json::json!("example.test"));
        let encoded = navigate.build_json(&args).unwrap();
        let invocation = ui::tilo_actions::InvocationId::new();
        assert_eq!(
            w.start_resource_operation(
                invocation,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &root,
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.test")
        );
        w.sync(&mut doc, &root);
        assert!(matches!(
            w.poll_resource_operation(invocation, false, &doc, &root),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));

        let mut filter_args = serde_json::Map::new();
        filter_args.insert("query".into(), serde_json::json!("tilo example"));
        let encoded = filter.build_json(&filter_args).unwrap();
        let started = w
            .start_resource_operation(
                ui::tilo_actions::InvocationId::new(),
                filter.kind,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &root,
            )
            .unwrap();
        assert!(matches!(
            started,
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(w.library_filter.get(), "tilo example");
    }

    #[test]
    fn sensitive_local_effects_publish_truthful_contracts_and_completed_results() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (doc, root) = synced(&mut w);
        w.history.record_at("https://private.example", "Private", 1);
        let resources = Widget::resources(&w, &doc);
        let browser = &resources[0];
        let history = browser
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.history")
            .unwrap();
        let clear_history = history
            .actions
            .iter()
            .find(|action| action.id == "clear")
            .unwrap();
        assert_eq!(clear_history.kind, OperationKind::Effect);
        assert_eq!(
            clear_history.locus,
            ui::tilo_actions::ExecutionLocus::ClientLocal
        );
        assert!(
            clear_history
                .contract
                .tags
                .iter()
                .any(|tag| tag == "device-local")
        );
        assert!(
            clear_history
                .contract
                .tags
                .iter()
                .any(|tag| tag == "sensitive-data")
        );
        assert!(clear_history.contract.validate_declaration().is_empty());

        let invocation = ui::tilo_actions::InvocationId::new();
        let started = w
            .start_resource_operation(
                invocation,
                clear_history.kind,
                clear_history.operation_type.as_deref().unwrap(),
                &browser_operation_payload(&BrowserOperation::ClearHistory)
                    .to_string()
                    .into_bytes(),
                &doc,
                &root,
            )
            .unwrap();
        let Some(ui::widget::ResourceOperationStart::Completed(output)) = started else {
            panic!("clearing in-memory local history is synchronous");
        };
        assert!(clear_history.contract.result.validate(&output).is_ok());
        assert!(w.history.is_empty());

        let settings = browser
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.settings")
            .unwrap();
        let clear_site_data = settings
            .actions
            .iter()
            .find(|action| action.id == "request-site-data-clear")
            .unwrap();
        let started = w
            .start_resource_operation(
                ui::tilo_actions::InvocationId::new(),
                clear_site_data.kind,
                clear_site_data.operation_type.as_deref().unwrap(),
                &browser_operation_payload(&BrowserOperation::RequestWebsiteDataClear)
                    .to_string()
                    .into_bytes(),
                &doc,
                &root,
            )
            .unwrap();
        let Some(ui::widget::ResourceOperationStart::Completed(output)) = started else {
            panic!("the engine accepted the site-data request synchronously");
        };
        assert!(clear_site_data.contract.result.validate(&output).is_ok());
        assert_eq!(
            output.value,
            Some(serde_json::Value::String(
                "Website-data clear request sent to the local browser engine.".into()
            )),
            "the descriptor must not overclaim a backend completion it cannot observe"
        );
        assert!(
            engine
                .commands()
                .contains(&WebViewCommand::ClearWebsiteData)
        );
    }

    #[test]
    fn history_pause_is_local_and_keeps_existing_history_searchable_and_clearable() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let (doc, root) = synced(&mut w);
        w.history
            .record_at("https://sensitive.example", "Sensitive", 1);
        let resources = Widget::resources(&w, &doc);
        let pause = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.session")
            .unwrap();
        assert_eq!(pause.kind, "browser.history-pause");
        let toggle = pause
            .actions
            .iter()
            .find(|action| action.id == "toggle-history-pause")
            .unwrap();
        assert_eq!(toggle.kind, OperationKind::Effect);
        assert_eq!(toggle.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
        assert!(
            toggle
                .contract
                .tags
                .iter()
                .any(|tag| tag == "history-paused")
        );
        assert!(toggle.contract.tags.iter().any(|tag| tag == "session"));
        assert!(toggle.contract.validate_declaration().is_empty());

        let started = w
            .start_resource_operation(
                ui::tilo_actions::InvocationId::new(),
                toggle.kind,
                toggle.operation_type.as_deref().unwrap(),
                &browser_operation_payload(&BrowserOperation::ToggleHistoryPause)
                    .to_string()
                    .into_bytes(),
                &doc,
                &root,
            )
            .unwrap();
        let Some(ui::widget::ResourceOperationStart::Completed(output)) = started else {
            panic!("history pausing changes only this running browser");
        };
        assert!(toggle.contract.result.validate(&output).is_ok());
        let resources = Widget::resources(&w, &doc);
        let history = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.history")
            .unwrap();
        assert_eq!(
            history.children.len(),
            1,
            "pausing only stops new recording"
        );
        assert!(
            history
                .actions
                .iter()
                .find(|action| action.id == "clear")
                .is_some_and(|action| action.contract.availability.is_available()),
            "existing durable history remains clearable while recording is paused"
        );
        w.search_index.sync(&w.history, &w.bookmarks);
        assert!(
            ui::search::SearchProvider::search(&w.search_index.provider(), "Sensitive")
                .iter()
                .any(|result| result.title == "Sensitive")
        );
    }

    #[test]
    fn ui_history_clear_preserves_entries_and_surfaces_a_persistence_failure() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), false);
        w.history_save = history_save_fails;
        let (mut doc, root) = synced(&mut w);
        w.history = History::new();
        w.history
            .record_at("https://durable.example", "Durable entry", 1);
        w.show_settings = true;

        // This is the settings-pane click-flag route, not a direct model
        // mutation: it must produce the same truthful outcome as the
        // canonical resource action.
        assert!(w.clear_history_durably().is_err());

        assert_eq!(w.history.len(), 1, "failed writes restore the old corpus");
        assert!(w.history_clear_error.as_deref().is_some_and(|message| {
            message.contains("could not save the change")
                && message.contains("Previous history was kept")
        }));

        let resources = Widget::resources(&w, &doc);
        let history = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.history")
            .unwrap();
        let clear = history
            .actions
            .iter()
            .find(|action| action.id == "clear")
            .unwrap();
        let error = w
            .start_resource_operation(
                ui::tilo_actions::InvocationId::new(),
                clear.kind,
                clear.operation_type.as_deref().unwrap(),
                &browser_operation_payload(&BrowserOperation::ClearHistory)
                    .to_string()
                    .into_bytes(),
                &doc,
                &root,
            )
            .expect_err("resource history clear must not complete without durable storage");
        assert!(error.contains("Previous history was kept"));
        assert_eq!(w.history.len(), 1);
    }

    #[test]
    fn picture_in_picture_host_path_waits_for_engine_success_or_error() {
        let engine = TestEngine::new();
        engine.set_media_element(Some(MediaElementInfo {
            element_id: "media:host".into(),
            viewport_rect: [0.1, 0.1, 0.6, 0.5],
            intrinsic_size: [1280, 720],
            label: "Host video".into(),
        }));
        let mut w = BrowserWidget::with_engine(engine, true);
        let (mut doc, root) = synced(&mut w);
        w.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        let resources = Widget::resources(&w, &doc);
        let page = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.page")
            .unwrap();
        let pip = page
            .actions
            .iter()
            .find(|action| action.id == "picture-in-picture")
            .unwrap();
        assert!(matches!(
            pip.contract.execution,
            ui::tilo_actions::ExecutionMode::Background { cancellable: false }
        ));
        let invocation = ui::tilo_actions::InvocationId::new();
        assert_eq!(
            w.start_resource_operation(
                invocation,
                pip.kind,
                pip.operation_type.as_deref().unwrap(),
                &browser_operation_payload(&BrowserOperation::TogglePictureInPicture)
                    .to_string()
                    .into_bytes(),
                &doc,
                &root,
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert!(
            w.poll_resource_operation(invocation, false, &doc, &root)
                .is_none()
        );
        w.sync(&mut doc, &root);
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            w.poll_resource_operation(invocation, false, &doc, &root)
        else {
            panic!("media acceptance must terminally succeed the admitted invocation");
        };
        assert!(pip.contract.result.validate(&output).is_ok());

        let mut unavailable = BrowserWidget::with_engine(TestEngine::new(), true);
        let (mut doc, root) = synced(&mut unavailable);
        let invocation = ui::tilo_actions::InvocationId::new();
        assert!(matches!(
            unavailable
                .start_resource_operation(
                    invocation,
                    pip.kind,
                    pip.operation_type.as_deref().unwrap(),
                    &browser_operation_payload(&BrowserOperation::TogglePictureInPicture)
                        .to_string()
                        .into_bytes(),
                    &doc,
                    &root,
                )
                .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        unavailable.sync(&mut doc, &root);
        assert!(matches!(
            unavailable.poll_resource_operation(invocation, false, &doc, &root),
            Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    #[test]
    fn bookmark_row_controls_use_unique_normalized_action_identities() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        w.debug_show_library("bookmarks");
        assert!(w.bookmarks.add("https://legacy.example", "Legacy bookmark"));
        let (mut doc, root) = synced(&mut w);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

        let expected = [
            bookmark_resource_id("https://www.rust-lang.org/learn"),
            bookmark_resource_id("https://developer.mozilla.org/"),
            bookmark_resource_id("https://legacy.example"),
        ];
        let controls = w
            .control_action_bindings()
            .into_iter()
            .filter_map(|binding| match binding.kind {
                ui::widget::ControlBindingKind::Action(address) if address.action == "remove" => {
                    Some((binding.control_id, address))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(controls.len(), expected.len());
        assert_eq!(
            controls
                .iter()
                .map(|(id, _)| id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            controls.len(),
            "each row needs a durable, unique semantic key"
        );
        for resource_id in expected {
            assert!(controls.iter().any(|(control_id, address)| {
                control_id == &format!("{resource_id}.remove") && address.resource == resource_id
            }));
        }

        let mut semantic = ui::a11y_tree::SemanticTree::new();
        semantic.refresh(&w, &node_map, None, &[]);
        assert!(
            semantic.duplicate_ids().is_empty(),
            "bookmark controls must not collapse in the retained a11y tree"
        );

        // The composed tree is not otherwise synchronized in this layout-only
        // regression test; keep the document borrow explicitly live.
        w.sync(&mut doc, &root);
    }

    #[test]
    fn continuation_restore_navigates_the_matching_browser_instance() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut widget);
        let metadata = ui::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes: br#"{"url":"https://example.test/reading"}"#.to_vec(),
        };
        assert!(widget.restore_continuation_metadata(&metadata, &mut doc, &root));
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.test/reading")
        );
    }

    #[test]
    fn portal_cancel_cancels_the_paused_engine_download() {
        ui::portal::suppress_for_tests();
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/archive.zip".into(),
        });
        widget.apply_engine_events();
        assert!(widget.download_picker.is_some());
        widget.apply_engine_events();
        assert_eq!(
            widget.downloads.entries()[0].status,
            DownloadStatus::Cancelled
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id } if id == "test-download-1"
        )));
    }

    #[test]
    fn portal_worker_failure_cancels_webkit_but_preserves_the_actionable_error() {
        ui::portal::suppress_for_tests();
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/archive.zip".into(),
        });
        widget.apply_engine_events();
        let id = widget.downloads.entries()[0].id.clone();
        let (sender, disconnected) = std::sync::mpsc::channel();
        drop(sender);
        widget.download_picker = Some((id.clone(), disconnected));
        widget.poll_download_picker();
        // Fold the engine's cancellation event too: it must not replace the
        // portal failure with the much less useful generic "Cancelled".
        widget.apply_engine_events();
        let entry = widget.downloads.get(&id).unwrap();
        assert_eq!(entry.status, DownloadStatus::Failed);
        assert!(
            entry
                .error
                .as_deref()
                .is_some_and(|error| error.contains("save picker"))
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id: cancelled } if cancelled == &id
        )));
    }

    #[test]
    fn duplicate_engine_ids_are_cancelled_without_replacing_history() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        assert!(widget.downloads.requested(
            "test-download-1".into(),
            "https://persisted.test/old".into(),
            "old.bin",
        ));
        engine.send(WebViewCommand::StartDownload {
            uri: "https://example.test/new.bin".into(),
        });
        widget.apply_engine_events();
        assert_eq!(
            widget.downloads.get("test-download-1").unwrap().source_url,
            "https://persisted.test/old"
        );
        assert!(engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::CancelDownload { id } if id == "test-download-1"
        )));
    }

    #[test]
    fn download_operations_reject_non_web_schemes_and_materialize_stable_actions() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        assert!(
            widget
                .perform_browser_operation(BrowserOperation::StartDownload {
                    url: "file:///etc/passwd".into(),
                })
                .is_err()
        );
        assert!(
            !engine
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::StartDownload { .. }))
        );

        widget.debug_show_downloads("completed");
        let resource = widget
            .resource_by_id("browser.download.review-complete")
            .expect("download resource");
        assert_eq!(resource.kind, "browser.download");
        assert_eq!(
            resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec!["show", "open", "reveal"]
        );
        for action in &resource.actions {
            assert_eq!(action.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
        }
    }
}
