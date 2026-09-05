//! `BrowserWidget` — the quick-ui root widget for the browser app.
//!
//! Chrome around the toolkit's [`ui::widget::WebView`]: a navigation
//! toolbar (back / forward / reload + address input), a thin progress
//! bar while a page loads, and the live page surface. There is no tab
//! strip — tabbing is the compositor's `WindowGroup` frame primitive, so
//! one browser window shows exactly one page.
//!
//! State model:
//!  * Address text lives in the CRDT under `browser.url` (Automerge
//!    `Text`) — the [`TextInput`] reads/writes it every frame, and the
//!    engine's `UriChanged` events are folded back into it (unless the
//!    user is currently editing).
//!  * Button presses surface as boolean flags (`browser.back_clicked`
//!    etc.); `sync` polls them, translates them into
//!    [`WebViewCommand`]s and resets them.
//!  * Everything engine-side (history, page pixels, load progress)
//!    stays in the engine; the widget only mirrors what the chrome
//!    displays.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::qos_actions::{
    ActionArgs, ActionBinding, ActionContract, ActionParam, ActionSpec, ControlInvocation,
    EncodedAction, OperationKind, ParamType, Resource, RetryPolicy,
};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::webview::test_engine::TestEngine;
use ui::webview::{MediaElementInfo, SharedEngine, WebViewCommand, WebViewEvent, normalise_url};
use ui::widget::toast::{ToastCard, ToastKind, paint_card};
use ui::widget::{
    AppKind, AppWidget, Button, Column, Container, EmptyState, IconDescriptor, ProgressBar, Row,
    ScrollAxis, Scrollable, Spacer, Text, TextInput, TextInputKind, WebView, Widget,
};

use crate::address_overlay::AddressSuggestions;
use crate::history::History;
use crate::library::Bookmarks;
use crate::settings::{Settings, SettingsPane};

/// CRDT path for the address-bar text.
const URL_PATH: &str = "browser.url";
/// CRDT flags the toolbar buttons write `true` to on click.
const BACK_FLAG: &str = "browser.back_clicked";
const FORWARD_FLAG: &str = "browser.forward_clicked";
const RELOAD_FLAG: &str = "browser.reload_clicked";
/// Gear button → toggle the settings pane.
const SETTINGS_FLAG: &str = "browser.settings_clicked";
const LIBRARY_FLAG: &str = "browser.library_clicked";
const BOOKMARK_FLAG: &str = "browser.bookmark_clicked";
const BOOKMARKS_PATH: &str = "browser.bookmarks";
const LIBRARY_HISTORY_FLAG: &str = "browser.library.history_clicked";
const LIBRARY_BOOKMARKS_FLAG: &str = "browser.library.bookmarks_clicked";
const LIBRARY_DOWNLOADS_FLAG: &str = "browser.library.downloads_clicked";
const LIBRARY_EXTENSIONS_FLAG: &str = "browser.library.extensions_clicked";
const HISTORY_OPEN_FLAGS: [&str; 12] = [
    "browser.library.history.open.0",
    "browser.library.history.open.1",
    "browser.library.history.open.2",
    "browser.library.history.open.3",
    "browser.library.history.open.4",
    "browser.library.history.open.5",
    "browser.library.history.open.6",
    "browser.library.history.open.7",
    "browser.library.history.open.8",
    "browser.library.history.open.9",
    "browser.library.history.open.10",
    "browser.library.history.open.11",
];
const BOOKMARK_OPEN_FLAGS: [&str; 12] = [
    "browser.library.bookmark.open.0",
    "browser.library.bookmark.open.1",
    "browser.library.bookmark.open.2",
    "browser.library.bookmark.open.3",
    "browser.library.bookmark.open.4",
    "browser.library.bookmark.open.5",
    "browser.library.bookmark.open.6",
    "browser.library.bookmark.open.7",
    "browser.library.bookmark.open.8",
    "browser.library.bookmark.open.9",
    "browser.library.bookmark.open.10",
    "browser.library.bookmark.open.11",
];
const BOOKMARK_REMOVE_FLAGS: [&str; 12] = [
    "browser.library.bookmark.remove.0",
    "browser.library.bookmark.remove.1",
    "browser.library.bookmark.remove.2",
    "browser.library.bookmark.remove.3",
    "browser.library.bookmark.remove.4",
    "browser.library.bookmark.remove.5",
    "browser.library.bookmark.remove.6",
    "browser.library.bookmark.remove.7",
    "browser.library.bookmark.remove.8",
    "browser.library.bookmark.remove.9",
    "browser.library.bookmark.remove.10",
    "browser.library.bookmark.remove.11",
];
const PIP_FLAG: &str = "browser.pip_clicked";
const PIP_ID: &str = "browser.media-pip";
const PIP_ELEMENT_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// How many address-bar suggestions to show.
const SUGGESTION_LIMIT: usize = 8;

/// e2e id of the address input (also used internally to detect whether
/// the user is editing it).
const URL_INPUT_ID: &str = "browser.url_input";
const PIP_FEEDBACK_LIFETIME: std::time::Duration = std::time::Duration::from_millis(4200);
const PIP_FEEDBACK_FADE: std::time::Duration = std::time::Duration::from_millis(180);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipFeedbackKind {
    Finding,
    NoVideo,
}

struct PipFeedback {
    kind: PipFeedbackKind,
    started: std::time::Instant,
}

const BROWSER_OPERATION_TYPE: &str = "browser.operation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibrarySection {
    History,
    Bookmarks,
    Downloads,
    Extensions,
}

#[derive(Clone, Debug)]
enum BrowserOperation {
    Back,
    Forward,
    Reload,
    Navigate { url: String },
    ShowLibrary { section: LibrarySection },
    ShowSettings,
    ShowPage,
    AddBookmark { url: String, title: String },
    RemoveBookmark { url: String },
    ClearHistory,
    ClearWebsiteData,
    SetAdblock { enabled: bool },
    SetClearOnExit { enabled: bool },
    SetHomepage { url: String },
    SetSearchEngine { template: String },
    TogglePictureInPicture,
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
        BrowserOperation::ShowSettings => serde_json::json!({ "operation": "show-settings" }),
        BrowserOperation::ShowPage => serde_json::json!({ "operation": "show-page" }),
        BrowserOperation::AddBookmark { url, title } => {
            serde_json::json!({ "operation": "add-bookmark", "url": url, "title": title })
        }
        BrowserOperation::RemoveBookmark { url } => {
            serde_json::json!({ "operation": "remove-bookmark", "url": url })
        }
        BrowserOperation::ClearHistory => serde_json::json!({ "operation": "clear-history" }),
        BrowserOperation::ClearWebsiteData => {
            serde_json::json!({ "operation": "clear-website-data" })
        }
        BrowserOperation::SetAdblock { enabled } => {
            serde_json::json!({ "operation": "set-adblock", "enabled": enabled })
        }
        BrowserOperation::SetClearOnExit { enabled } => {
            serde_json::json!({ "operation": "set-clear-on-exit", "enabled": enabled })
        }
        BrowserOperation::SetHomepage { url } => {
            serde_json::json!({ "operation": "set-homepage", "url": url })
        }
        BrowserOperation::SetSearchEngine { template } => {
            serde_json::json!({ "operation": "set-search-engine", "template": template })
        }
        BrowserOperation::TogglePictureInPicture => {
            serde_json::json!({ "operation": "toggle-picture-in-picture" })
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
        Some("show-settings") => Ok(BrowserOperation::ShowSettings),
        Some("show-page") => Ok(BrowserOperation::ShowPage),
        Some("add-bookmark") => Ok(BrowserOperation::AddBookmark {
            url: value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: value
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        Some("remove-bookmark") => Ok(BrowserOperation::RemoveBookmark {
            url: value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        Some("clear-history") => Ok(BrowserOperation::ClearHistory),
        Some("clear-website-data") => Ok(BrowserOperation::ClearWebsiteData),
        Some("set-adblock") => Ok(BrowserOperation::SetAdblock {
            enabled: value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }),
        Some("set-clear-on-exit") => Ok(BrowserOperation::SetClearOnExit {
            enabled: value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }),
        Some("set-homepage") => Ok(BrowserOperation::SetHomepage {
            url: value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into(),
        }),
        Some("set-search-engine") => Ok(BrowserOperation::SetSearchEngine {
            template: value
                .get("template")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into(),
        }),
        Some("toggle-picture-in-picture") => Ok(BrowserOperation::TogglePictureInPicture),
        _ => Err("unknown browser operation".to_string()),
    }
}

fn navigation_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        execution: ui::qos_actions::ExecutionMode::Background { cancellable: false },
        tags: vec!["network".into(), "navigation".into()],
        ..ActionContract::view()
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
    forced_load_error: bool,

    // User-facing browser state.
    settings: Settings,
    settings_dirty: bool,
    history: History,
    bookmarks: Bookmarks,
    bookmarks_dirty: bool,
    /// Fuzzy suggestion dropdown under the address bar (a field, not a
    /// tree child: driven here and painted in `draw_overlay`).
    suggestions: AddressSuggestions,
    /// Whether the settings pane replaces the page surface.
    show_settings: bool,
    show_library: bool,
    library_section: LibrarySection,
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
    resource_navigation_invocation: Option<ui::qos_actions::InvocationId>,
    resource_navigation_result: Option<ui::qos_actions::InvocationUpdate>,
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
        Self {
            rect: Rect::ZERO,
            engine,
            composed: None,
            bootstrap_done: false,
            headless,
            initial_url: DEFAULT_URL.into(),
            can_go_back: false,
            can_go_forward: false,
            progress: 1.0,
            title: String::new(),
            current_uri: String::new(),
            load_error: None,
            forced_load_error: false,
            settings,
            settings_dirty: false,
            history,
            bookmarks,
            bookmarks_dirty: false,
            suggestions: AddressSuggestions::new(),
            show_settings: false,
            show_library: false,
            library_section: LibrarySection::History,
            structure_dirty: false,
            last_query: None,
            force_suggestions: false,
            content_rect: std::sync::Arc::new(std::sync::Mutex::new(None)),
            resource_navigation_invocation: None,
            resource_navigation_result: None,
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
        self.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now() - std::time::Duration::from_millis(500),
        });
    }

    /// Headless visual-review hook for the toolbar's adopted PiP state.
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
        self.title = "Couldn’t open this page".into();
        self.load_error =
            Some("The site did not respond. Check your connection and try again.".into());
        self.forced_load_error = true;
    }

    /// Seed the responsive breakpoint before a single-frame offscreen render.
    pub fn debug_set_viewport_width(&mut self, width: u32) {
        self.compact_chrome = width < 600;
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

    pub fn debug_show_settings(&mut self) {
        self.show_settings = true;
        self.show_library = false;
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

    fn perform_browser_operation(&mut self, operation: BrowserOperation) {
        match operation {
            BrowserOperation::Back => self.engine.send(WebViewCommand::GoBack),
            BrowserOperation::Forward => self.engine.send(WebViewCommand::GoForward),
            BrowserOperation::Reload => self.engine.send(WebViewCommand::Reload),
            BrowserOperation::Navigate { url } => {
                self.show_library = false;
                self.show_settings = false;
                self.structure_dirty = true;
                self.engine
                    .send(WebViewCommand::Navigate(self.navigation_target(url.trim())));
            }
            BrowserOperation::ShowLibrary { section } => {
                self.library_section = section;
                self.show_library = true;
                self.show_settings = false;
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
            BrowserOperation::AddBookmark { url, title } => {
                if self.bookmarks.add(&url, &title) && !self.headless {
                    let _ = self.bookmarks.save();
                }
                self.bookmarks_dirty = true;
                self.structure_dirty = true;
            }
            BrowserOperation::RemoveBookmark { url } => {
                if self.bookmarks.remove(&url) && !self.headless {
                    let _ = self.bookmarks.save();
                }
                self.bookmarks_dirty = true;
                self.structure_dirty = true;
            }
            BrowserOperation::ClearHistory => {
                self.history.clear();
                if !self.headless {
                    let _ = self.history.save();
                }
                self.structure_dirty = true;
            }
            BrowserOperation::ClearWebsiteData => {
                self.engine.send(WebViewCommand::ClearWebsiteData);
            }
            BrowserOperation::SetAdblock { enabled } => {
                self.settings.adblock_enabled = enabled;
                self.set_adblock(enabled);
                if !self.headless {
                    self.settings.save();
                }
                self.settings_dirty = true;
            }
            BrowserOperation::SetClearOnExit { enabled } => {
                self.settings.clear_on_exit = enabled;
                if !self.headless {
                    self.settings.save();
                }
                self.settings_dirty = true;
            }
            BrowserOperation::SetHomepage { url } => {
                if url::Url::parse(&url).is_ok() {
                    self.settings.homepage = url;
                    if !self.headless {
                        self.settings.save();
                    }
                    self.settings_dirty = true;
                }
            }
            BrowserOperation::SetSearchEngine { template } => {
                if url::Url::parse(&template).is_ok() {
                    self.settings.search_engine = template;
                    if !self.headless {
                        self.settings.save();
                    }
                    self.settings_dirty = true;
                }
            }
            BrowserOperation::TogglePictureInPicture => self.begin_pip_probe(),
        }
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

    fn bind_button(&self, button: Button, resource: &str, action: &str) -> Button {
        match self.unit_binding(resource, action) {
            Some(binding) => button.on_action(binding),
            None => button,
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

    fn navigation_target(&self, input: &str) -> String {
        let input = input.trim();
        if url::Url::parse(input)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https" | "file"))
            || (!input.chars().any(char::is_whitespace) && input.contains('.'))
        {
            return normalise_url(input);
        }
        let query: String = url::form_urlencoded::byte_serialize(input.as_bytes()).collect();
        if self.settings.search_engine.contains("{query}") {
            self.settings.search_engine.replace("{query}", &query)
        } else {
            format!("{}{query}", self.settings.search_engine)
        }
    }

    fn page_rect(&self) -> Rect {
        self.content_rect
            .lock()
            .ok()
            .and_then(|r| *r)
            .map(|[x0, y0, x1, y1]| {
                Rect::new(
                    self.rect.x0 + f64::from(x0) * self.rect.width(),
                    self.rect.y0 + f64::from(y0) * self.rect.height(),
                    self.rect.x0 + f64::from(x1) * self.rect.width(),
                    self.rect.y0 + f64::from(y1) * self.rect.height(),
                )
            })
            .unwrap_or(self.rect)
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
        self.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::Finding,
            started: std::time::Instant::now(),
        });
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
            self.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: std::time::Instant::now(),
            });
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
                app_id: "dev.qos.browser".into(),
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
        self.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now(),
        });
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
                app_id: "dev.qos.browser".into(),
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
        self.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now(),
        });
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // Compact, square-ish icon buttons (44×40 — a comfortable touch
        // target) instead of the toolkit's 120×40 label-button default,
        // so three of them leave the address bar the rest of the width
        // even on a narrow / mobile-width window.
        const NAV_BTN: (f32, f32) = (44.0, 40.0);
        let back = self.bind_button(
            Button::new("browser.back", "", Some(BACK_FLAG))
                .with_icon(Icon::ArrowLeft)
                .tooltip("Back")
                .with_size(NAV_BTN.0, NAV_BTN.1)
                .with_enabled(self.can_go_back),
            "browser.page",
            "back",
        );
        let forward = self.bind_button(
            Button::new("browser.forward", "", Some(FORWARD_FLAG))
                .with_icon(Icon::ArrowRight)
                .tooltip("Forward")
                .with_size(NAV_BTN.0, NAV_BTN.1)
                .with_enabled(self.can_go_forward),
            "browser.page",
            "forward",
        );
        let reload = self.bind_button(
            Button::new("browser.reload", "", Some(RELOAD_FLAG))
                .with_icon(Icon::Refresh)
                .tooltip("Reload")
                .with_size(NAV_BTN.0, NAV_BTN.1),
            "browser.page",
            "reload",
        );
        let url_input = TextInput::new(vec![])
            .with_kind(TextInputKind::Url)
            .with_id(URL_INPUT_ID)
            .with_on_change_path(URL_PATH)
            .with_accessible_name("Address and search")
            .with_placeholder("Search or enter address");
        let settings_btn = self.bind_button(
            Button::new("browser.settings", "", Some(SETTINGS_FLAG))
                .with_icon(Icon::Settings)
                .tooltip("Browser settings")
                .with_size(NAV_BTN.0, NAV_BTN.1),
            "browser.settings",
            "show",
        );
        let pip_label = if self.active_pip {
            "Close Picture in Picture"
        } else {
            "Picture in Picture"
        };
        let pip_btn = self.bind_button(
            Button::new("browser.pip", "", Some(PIP_FLAG))
                .with_icon(Icon::PictureInPicture2)
                .with_accessible_name(pip_label)
                .tooltip(pip_label)
                .with_size(NAV_BTN.0, NAV_BTN.1),
            "browser.page",
            "picture-in-picture",
        );
        let pip_btn = if self.active_pip {
            pip_btn.primary()
        } else {
            pip_btn
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
            Button::new("browser.bookmark", "", Some(BOOKMARK_FLAG))
                .with_icon(Icon::Star)
                .with_accessible_name(bookmark_label)
                .tooltip(bookmark_label)
                .with_enabled(!self.current_uri.trim().is_empty())
                .with_size(NAV_BTN.0, NAV_BTN.1),
            "browser.page",
            "bookmark",
        );
        let library_btn = self.bind_button(
            Button::new("browser.library", "", Some(LIBRARY_FLAG))
                .with_icon(Icon::History)
                .with_accessible_name(if self.show_library {
                    "Close browser library"
                } else {
                    "Open browser library"
                })
                .tooltip("History, bookmarks and downloads")
                .with_size(NAV_BTN.0, NAV_BTN.1),
            "browser.history",
            "show",
        );
        toolbar = toolbar
            .push(Box::new(bookmark_btn))
            .push(Box::new(library_btn));
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
        if !self.show_settings && self.progress < 1.0 {
            column = column.push(Box::new(ProgressBar::new(self.progress as f32)));
        }
        // The content region is either the settings pane or the live page.
        let content: Box<dyn Widget> = if self.show_settings {
            Box::new(SettingsPane::with_settings(self.settings.clone()))
        } else if self.show_library {
            self.build_library_composition()
        } else if let Some(error) = &self.load_error {
            Box::new(
                EmptyState::new("Couldn’t open this page")
                    .with_icon(Icon::Wifi)
                    .with_message(error)
                    .with_action("Try again")
                    .with_action_id("browser.retry")
                    .with_action_path(RELOAD_FLAG),
            )
        } else {
            Box::new(WebView::new(self.engine.clone()).with_test_id("browser.surface"))
        };
        column = column.push(content);
        Box::new(column)
    }

    fn build_library_composition(&self) -> Box<dyn Widget> {
        let heading = Row::new()
            .push(Box::new(Text::new("Library").with_size(24.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                self.bind_button(
                    Button::new("browser.library.close", "Done", Some(LIBRARY_FLAG))
                        .with_size(96.0, 36.0),
                    "browser",
                    "show-page",
                ),
            ));
        let mut sections = Row::new();
        for (id, label, flag, selected, resource) in [
            (
                "browser.library.history",
                "History",
                LIBRARY_HISTORY_FLAG,
                self.library_section == LibrarySection::History,
                "browser.history",
            ),
            (
                "browser.library.bookmarks",
                "Bookmarks",
                LIBRARY_BOOKMARKS_FLAG,
                self.library_section == LibrarySection::Bookmarks,
                "browser.bookmarks",
            ),
            (
                "browser.library.downloads",
                "Downloads",
                LIBRARY_DOWNLOADS_FLAG,
                self.library_section == LibrarySection::Downloads,
                "browser.downloads",
            ),
            (
                "browser.library.extensions",
                "Extensions",
                LIBRARY_EXTENSIONS_FLAG,
                self.library_section == LibrarySection::Extensions,
                "browser.extensions",
            ),
        ] {
            let button = self.bind_button(
                Button::new(id, label, Some(flag))
                    .with_accessible_name(&format!("Show {label}"))
                    .with_size(132.0, 40.0),
                resource,
                "show",
            );
            sections = sections.push(Box::new(if selected { button.primary() } else { button }));
        }

        let body: Box<dyn Widget> = match self.library_section {
            LibrarySection::History if self.history.is_empty() => Box::new(
                EmptyState::new("No browsing history yet")
                    .with_icon(Icon::History)
                    .with_message("Pages you visit appear here. History stays on this device."),
            ),
            LibrarySection::History => {
                let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                for (index, entry) in self.history.entries().iter().rev().take(12).enumerate() {
                    let resource_id = format!(
                        "browser.history.{}",
                        blake3::hash(entry.url.as_bytes()).to_hex()
                    );
                    let title = if entry.title.trim().is_empty() {
                        entry.url.as_str()
                    } else {
                        entry.title.as_str()
                    };
                    rows = rows
                        .push(Box::new(
                            Container::new().with_background().with_child(Box::new(
                                Row::new()
                                    .push(Box::new(
                                        Column::new()
                                            .with_no_padding()
                                            .with_intrinsic_height()
                                            .push(Box::new(Text::new(title).with_size(15.0)))
                                            .push(Box::new(
                                                Text::new(&entry.url).with_size(12.0).dimmed(),
                                            )),
                                    ))
                                    .push(Box::new(Spacer::new()))
                                    .push(Box::new(
                                        self.bind_button(
                                            Button::new(
                                                HISTORY_OPEN_FLAGS[index],
                                                "Open",
                                                Some(HISTORY_OPEN_FLAGS[index]),
                                            )
                                            .with_accessible_name(&format!("Open {title}"))
                                            .tooltip("Open page")
                                            .with_size(84.0, 36.0),
                                            &resource_id,
                                            "open",
                                        ),
                                    )),
                            )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_height(10.0),
                        ));
                }
                Box::new(rows)
            }
            LibrarySection::Bookmarks if self.bookmarks.entries().is_empty() => Box::new(
                EmptyState::new("No bookmarks yet")
                    .with_icon(Icon::Star)
                    .with_message("Use the star in the address bar to keep a page."),
            ),
            LibrarySection::Bookmarks => {
                let mut rows = Column::new().with_no_padding().with_intrinsic_height();
                for (index, entry) in self.bookmarks.entries().iter().take(12).enumerate() {
                    let resource_id = format!(
                        "browser.bookmark.{}",
                        blake3::hash(entry.url.as_bytes()).to_hex()
                    );
                    rows = rows
                        .push(Box::new(
                            Container::new().with_background().with_child(Box::new(
                                Row::new()
                                    .push(Box::new(
                                        Column::new()
                                            .with_no_padding()
                                            .with_intrinsic_height()
                                            .push(Box::new(Text::new(&entry.title).with_size(15.0)))
                                            .push(Box::new(
                                                Text::new(&entry.url).with_size(12.0).dimmed(),
                                            )),
                                    ))
                                    .push(Box::new(Spacer::new()))
                                    .push(Box::new(
                                        self.bind_button(
                                            Button::new(
                                                BOOKMARK_OPEN_FLAGS[index],
                                                "Open",
                                                Some(BOOKMARK_OPEN_FLAGS[index]),
                                            )
                                            .with_accessible_name(&format!("Open {}", entry.title))
                                            .with_size(84.0, 36.0),
                                            &resource_id,
                                            "open",
                                        ),
                                    ))
                                    .push(Box::new(
                                        self.bind_button(
                                            Button::new(
                                                BOOKMARK_REMOVE_FLAGS[index],
                                                "Remove",
                                                Some(BOOKMARK_REMOVE_FLAGS[index]),
                                            )
                                            .with_accessible_name(&format!(
                                                "Remove bookmark {}",
                                                entry.title
                                            ))
                                            .tooltip("Remove bookmark")
                                            .with_size(96.0, 36.0),
                                            &resource_id,
                                            "remove",
                                        ),
                                    )),
                            )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_height(10.0),
                        ));
                }
                Box::new(rows)
            }
            LibrarySection::Downloads => Box::new(
                EmptyState::new("No downloads in this window")
                    .with_icon(Icon::Download)
                    .with_message(
                        "Saved files appear in Files. Transfer progress is not available yet.",
                    ),
            ),
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
                    for extension in installed.into_iter().take(100) {
                        let status = format!("Version {} · installed, inactive", extension.version);
                        rows = rows
                            .push(Box::new(
                                Container::new().with_background().with_child(Box::new(
                                    Column::new()
                                        .with_no_padding()
                                        .with_intrinsic_height()
                                        .push(Box::new(Text::new(&extension.name).with_size(15.0)))
                                        .push(Box::new(
                                            Text::new(&status).with_size(12.0).dimmed(),
                                        )),
                                )),
                            ))
                            .push(Box::new(
                                Container::new().with_no_padding().with_height(10.0),
                            ));
                    }
                    Box::new(rows)
                }
            }
        };

        let content = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(heading))
            .push(Box::new(
                Container::new().with_no_padding().with_height(18.0),
            ))
            .push(Box::new(sections))
            .push(Box::new(
                Container::new().with_no_padding().with_height(18.0),
            ))
            .push(body);
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(Container::new().with_child(Box::new(content)))),
        )
    }

    /// One-shot CRDT bootstrap so children can read/write their paths
    /// on the first frame.
    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, URL_PATH).ok().flatten().is_none()
            && let Ok(obj_id) = state.put_object(root, URL_PATH, ObjType::Text)
        {
            let _ = state.splice_text(&obj_id, 0, 0, &self.initial_url);
        }
        if state.get(root, BOOKMARKS_PATH).ok().flatten().is_none() {
            let _ = state.put(root, BOOKMARKS_PATH, self.bookmarks.to_json());
        }
        for flag in [
            BACK_FLAG,
            FORWARD_FLAG,
            RELOAD_FLAG,
            SETTINGS_FLAG,
            LIBRARY_FLAG,
            BOOKMARK_FLAG,
            LIBRARY_HISTORY_FLAG,
            LIBRARY_BOOKMARKS_FLAG,
            LIBRARY_DOWNLOADS_FLAG,
            LIBRARY_EXTENSIONS_FLAG,
            PIP_FLAG,
        ] {
            if state.get(root, flag).ok().flatten().is_none() {
                let _ = state.put(root, flag, false);
            }
        }
    }

    fn read_url(state: &AutoCommit, root: &ObjId) -> Option<String> {
        if let Ok(Some((Value::Object(ObjType::Text), obj_id))) = state.get(root, URL_PATH) {
            state.text(&obj_id).ok()
        } else {
            None
        }
    }

    fn sync_bookmarks(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.bookmarks_dirty {
            let _ = state.put(root, BOOKMARKS_PATH, self.bookmarks.to_json());
            self.bookmarks_dirty = false;
            return;
        }
        let Some(remote) = state
            .get(root, BOOKMARKS_PATH)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_scalar().ok())
            .and_then(|value| value.to_str().map(str::to_string))
        else {
            return;
        };
        let incoming = Bookmarks::from_json(&remote);
        if incoming != self.bookmarks {
            self.bookmarks = incoming;
            if !self.headless {
                let _ = self.bookmarks.save();
            }
            self.structure_dirty = true;
        }
    }

    fn publish_settings_if_dirty(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !std::mem::take(&mut self.settings_dirty) {
            return;
        }
        let _ = state.put(
            root,
            crate::settings::ADBLOCK_PATH,
            self.settings.adblock_enabled,
        );
        let _ = state.put(
            root,
            crate::settings::CLEAR_ON_EXIT_PATH,
            self.settings.clear_on_exit,
        );
        ui::put_text_diffed(
            state,
            root,
            crate::settings::HOMEPAGE_PATH,
            &self.settings.homepage,
        );
        ui::put_text_diffed(
            state,
            root,
            crate::settings::SEARCH_ENGINE_PATH,
            &self.settings.search_engine,
        );
    }

    fn continuation_url(state: &AutoCommit, root: &ObjId) -> Option<String> {
        let url = Self::read_url(state, root)?;
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

    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        )
    }

    /// Take a raised flag, returning whether it was set (and resetting
    /// it so one click fires exactly once).
    fn take_flag(state: &mut AutoCommit, root: &ObjId, path: &str) -> bool {
        if Self::read_flag(state, root, path) {
            let _ = state.put(root, path, false);
            true
        } else {
            false
        }
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

    /// Fold engine events into chrome state + the CRDT address text.
    fn apply_engine_events(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let editing = self.url_input_focused();
        for event in self.engine.drain_events() {
            match event {
                WebViewEvent::UriChanged(uri) => {
                    if !self.forced_load_error {
                        self.load_error = None;
                    }
                    if !editing {
                        ui::put_text_diffed(state, root, URL_PATH, &uri);
                    }
                    self.current_uri = uri;
                }
                WebViewEvent::TitleChanged(title) => self.title = title,
                WebViewEvent::LoadProgress(p) => self.progress = p,
                WebViewEvent::LoadFinished => {
                    if !self.forced_load_error {
                        self.load_error = None;
                    }
                    self.progress = 1.0;
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if !self.headless && self.current_uri.starts_with("http") {
                        self.history.record(&self.current_uri, &self.title);
                        let _ = self.history.save();
                    }
                    if let Some(invocation) = self.resource_navigation_invocation.take() {
                        self.resource_navigation_result =
                            Some(ui::qos_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: ui::qos_actions::ActionOutput::default(),
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
                        self.resource_navigation_result =
                            Some(ui::qos_actions::InvocationUpdate::Failed {
                                invocation,
                                error: ui::qos_actions::ActionFailure {
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
            }
        }
    }

    /// Navigate to whatever the address bar currently holds.
    fn navigate_to_input(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(url) = Self::read_url(state, root) {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                self.queue_navigate(trimmed.to_string());
            }
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

    /// Poll the settings pane's one-shot + level intents and act on them.
    fn apply_settings_intents(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let intents = SettingsPane::take_pending(state, root);
        if intents.close && self.show_settings {
            self.show_settings = false;
            self.structure_dirty = true;
        }
        if intents.clear_cookies {
            self.engine.send(WebViewCommand::ClearWebsiteData);
        }
        if intents.clear_history {
            self.history.clear();
            let _ = self.history.save();
            self.suggestions.clear();
            self.last_query = None;
        }
        let mut changed = false;
        if let Some(v) = intents.adblock_enabled
            && v != self.settings.adblock_enabled
        {
            self.settings.adblock_enabled = v;
            self.set_adblock(v);
            changed = true;
        }
        if let Some(v) = intents.clear_on_exit
            && v != self.settings.clear_on_exit
        {
            self.settings.clear_on_exit = v;
            changed = true;
        }
        if let Some(value) = intents.search_engine {
            let value = value.trim();
            if !value.is_empty() && value != self.settings.search_engine {
                self.settings.search_engine = value.to_string();
                changed = true;
            }
        }
        if let Some(value) = intents.homepage {
            let value = value.trim();
            if url::Url::parse(value).is_ok() && value != self.settings.homepage {
                self.settings.homepage = value.to_string();
                changed = true;
            }
        }
        if changed && !self.headless {
            self.settings.save();
        }
    }

    /// Refresh the address-bar suggestion list from history while the
    /// address input is focused; hide it otherwise.
    fn update_suggestions(&mut self, state: &AutoCommit, root: &ObjId) {
        if self.force_suggestions {
            return; // headless overlay render — keep the pinned list
        }
        let text = if self.show_settings || !self.url_input_focused() {
            String::new()
        } else {
            Self::read_url(state, root).unwrap_or_default()
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

    /// The icon-only toolbar buttons intentionally have empty visible labels,
    /// so their generic widget test id is not useful as an anchor. The PiP
    /// affordance is the penultimate toolbar button (settings is last).
    fn pip_button_rect(&self) -> Option<Rect> {
        fn collect(w: &dyn Widget, rects: &mut Vec<Rect>) {
            if w.name() == "Button" && w.get_rect().area() > 0.0 {
                rects.push(w.get_rect());
            }
            for child in w.children() {
                collect(child, rects);
            }
        }
        let mut rects = Vec::new();
        if let Some(composed) = &self.composed {
            collect(composed.as_ref(), &mut rects);
        }
        rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        rects.get(rects.len().checked_sub(2)?).copied()
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

impl Widget for BrowserWidget {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let url = Self::continuation_url(state, root)?;
        let bytes = serde_json::to_vec(&serde_json::json!({ "url": url })).ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut AutoCommit,
        root: &ObjId,
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
        ui::put_text_diffed(state, root, URL_PATH, url);
        self.initial_url = url.to_owned();
        if self.bootstrap_done {
            self.engine.send(WebViewCommand::Navigate(url.to_owned()));
        }
        true
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[BROWSER_OPERATION_TYPE]
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        const MATERIALIZED_LIMIT: usize = 100;
        let resource_id = |kind: &str, url: &str| {
            format!("browser.{kind}.{}", blake3::hash(url.as_bytes()).to_hex())
        };
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
        if let Some(spec) = operation(
            "bookmark",
            "Bookmark this page",
            OperationKind::Effect,
            BrowserOperation::AddBookmark {
                url: self.current_uri.clone(),
                title: self.title.clone(),
            },
        ) {
            page.actions.push(if self.current_uri.trim().is_empty() {
                spec.unavailable("no_page", "Open a page before bookmarking it")
            } else if self.bookmarks.contains(&self.current_uri) {
                spec.unavailable("already_bookmarked", "This page is already bookmarked")
            } else {
                spec
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
            page.actions.push(spec);
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
                spec.unavailable("already_empty", "Browsing history is already empty")
            } else {
                spec.destructive()
            });
        }
        for entry in self.history.entries().iter().rev().take(MATERIALIZED_LIMIT) {
            let title = if entry.title.trim().is_empty() {
                entry.url.as_str()
            } else {
                entry.title.as_str()
            };
            let mut child = Resource::new(
                resource_id("history", &entry.url),
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
                resource_id("bookmark", &entry.url),
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
            if let Some(spec) = operation(
                "remove",
                "Remove bookmark",
                OperationKind::Effect,
                BrowserOperation::RemoveBookmark {
                    url: entry.url.clone(),
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
            "clear-site-data",
            "Clear cookies and site data",
            OperationKind::Effect,
            BrowserOperation::ClearWebsiteData,
        ) {
            settings.actions.push(spec.destructive());
        }
        for (id, label, operation) in [
            ("set-adblock", "Set ad and tracker blocking", "set-adblock"),
            (
                "set-clear-on-exit",
                "Set clear data on exit",
                "set-clear-on-exit",
            ),
        ] {
            settings.actions.push(ActionSpec::typed_operation(
                id,
                label,
                "enabled",
                OperationKind::Effect,
                BROWSER_OPERATION_TYPE,
                vec![ActionParam::required_owned(
                    "enabled",
                    ParamType::Boolean,
                    "Whether the setting is enabled",
                )],
                move |args: &ActionArgs| {
                    let operation = if operation == "set-adblock" {
                        BrowserOperation::SetAdblock {
                            enabled: args.boolean("enabled").unwrap_or(false),
                        }
                    } else {
                        BrowserOperation::SetClearOnExit {
                            enabled: args.boolean("enabled").unwrap_or(false),
                        }
                    };
                    encode_browser_operation(&operation)
                },
            ));
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
            settings.actions.push(ActionSpec::typed_operation(
                id,
                label,
                parameter,
                OperationKind::Effect,
                BROWSER_OPERATION_TYPE,
                vec![ActionParam::required_owned(
                    parameter,
                    ParamType::Text,
                    parameter,
                )],
                move |args: &ActionArgs| {
                    let operation = if operation == "homepage" {
                        BrowserOperation::SetHomepage {
                            url: args.text(parameter).unwrap_or_default().into(),
                        }
                    } else {
                        BrowserOperation::SetSearchEngine {
                            template: args.text(parameter).unwrap_or_default().into(),
                        }
                    };
                    encode_browser_operation(&operation)
                },
            ));
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
        browser.children = vec![page, history, bookmarks, downloads, extensions, settings];
        vec![browser]
    }

    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::BrowserSearchProvider::new(&self.history, &self.bookmarks),
        )]
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
        self.perform_browser_operation(operation);
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: ui::qos_actions::InvocationId,
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
        if !is_navigation {
            self.perform_browser_operation(operation);
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                ui::qos_actions::ActionOutput::default(),
            )));
        }
        if self.resource_navigation_invocation.is_some() {
            return Err("another browser navigation is already running".into());
        }
        self.perform_browser_operation(operation);
        self.resource_navigation_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: ui::qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::qos_actions::InvocationUpdate> {
        if self
            .resource_navigation_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_navigation_result.take()
        } else {
            None
        }
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, path);
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

        self.apply_engine_events(state, path);

        if Self::take_flag(state, path, BACK_FLAG) {
            self.perform_browser_operation(BrowserOperation::Back);
        }
        if Self::take_flag(state, path, FORWARD_FLAG) {
            self.perform_browser_operation(BrowserOperation::Forward);
        }
        if Self::take_flag(state, path, RELOAD_FLAG) {
            self.perform_browser_operation(BrowserOperation::Reload);
        }
        if Self::take_flag(state, path, SETTINGS_FLAG) {
            self.show_settings = !self.show_settings;
            self.show_library = false;
            self.structure_dirty = true;
            self.suggestions.clear();
        }
        if Self::take_flag(state, path, LIBRARY_FLAG) {
            self.show_library = !self.show_library;
            self.show_settings = false;
            self.structure_dirty = true;
            self.suggestions.clear();
        }
        if Self::take_flag(state, path, BOOKMARK_FLAG) && !self.current_uri.trim().is_empty() {
            self.perform_browser_operation(BrowserOperation::AddBookmark {
                url: self.current_uri.clone(),
                title: self.title.clone(),
            });
        }
        for (flag, section) in [
            (LIBRARY_HISTORY_FLAG, LibrarySection::History),
            (LIBRARY_BOOKMARKS_FLAG, LibrarySection::Bookmarks),
            (LIBRARY_DOWNLOADS_FLAG, LibrarySection::Downloads),
            (LIBRARY_EXTENSIONS_FLAG, LibrarySection::Extensions),
        ] {
            if Self::take_flag(state, path, flag) {
                self.library_section = section;
                self.structure_dirty = true;
            }
        }
        let history_open = self
            .history
            .entries()
            .iter()
            .rev()
            .take(HISTORY_OPEN_FLAGS.len())
            .enumerate()
            .find_map(|(index, entry)| {
                Self::take_flag(state, path, HISTORY_OPEN_FLAGS[index]).then(|| entry.url.clone())
            });
        if let Some(url) = history_open {
            self.perform_browser_operation(BrowserOperation::Navigate { url });
        }
        let bookmark_operation = self
            .bookmarks
            .entries()
            .iter()
            .take(BOOKMARK_OPEN_FLAGS.len())
            .enumerate()
            .find_map(|(index, entry)| {
                if Self::take_flag(state, path, BOOKMARK_OPEN_FLAGS[index]) {
                    Some(BrowserOperation::Navigate {
                        url: entry.url.clone(),
                    })
                } else if Self::take_flag(state, path, BOOKMARK_REMOVE_FLAGS[index]) {
                    Some(BrowserOperation::RemoveBookmark {
                        url: entry.url.clone(),
                    })
                } else {
                    None
                }
            });
        if let Some(operation) = bookmark_operation {
            self.perform_browser_operation(operation);
        }
        if Self::take_flag(state, path, "browser.library.history.clear") {
            self.perform_browser_operation(BrowserOperation::ClearHistory);
        }
        if Self::take_flag(state, path, PIP_FLAG) && !self.show_settings && !self.show_library {
            self.begin_pip_probe();
        }

        self.apply_settings_intents(state, path);
        self.publish_settings_if_dirty(state, path);
        self.sync_bookmarks(state, path);
        self.update_suggestions(state, path);

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
        let mut composed = self.build_composition();
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
                feedback.kind == PipFeedbackKind::NoVideo
                    && feedback.started.elapsed() >= PIP_FEEDBACK_LIFETIME
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
            if feedback.kind == PipFeedbackKind::NoVideo && !feedback_hovered {
                let remaining = PIP_FEEDBACK_LIFETIME.saturating_sub(elapsed);
                alpha =
                    alpha.min((remaining.as_secs_f32() / PIP_FEEDBACK_FADE.as_secs_f32()).min(1.0));
            }
            if let Some(anchor) = self.pip_button_rect() {
                let has_actions = feedback.kind == PipFeedbackKind::NoVideo;
                let desired_width: f64 = if has_actions { 354.0 } else { 326.0 };
                let width = desired_width.min((self.rect.width() - 24.0).max(220.0));
                let x1 = (anchor.x1 + 2.0).min(self.rect.x1 - 12.0);
                let height = if has_actions { 112.0 } else { 64.0 };
                let rect = Rect::new(x1 - width, anchor.y1 + 8.0, x1, anchor.y1 + 8.0 + height);
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
                ui::put_text_diffed(state, path, URL_PATH, &entry.url);
                self.queue_navigate(entry.url);
                *handled = true;
                return;
            }
            if ov_handled {
                *handled = true;
                return;
            }
        }

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

        // Enter in the focused address bar submits it (no suggestion picked).
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && key.logical_key == WKey::Named(WNamedKey::Enter)
            && self.url_input_focused()
        {
            self.navigate_to_input(state, path);
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
        self.pending_controls.pop_front()
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
        assert_eq!(
            BrowserWidget::read_url(&state, &root).as_deref(),
            Some(DEFAULT_URL)
        );
    }

    #[test]
    fn button_flags_translate_to_engine_commands() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // History so back/forward are meaningful.
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        let _ = state.put(&root, BACK_FLAG, true);
        w.sync(&mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        assert!(
            !BrowserWidget::read_flag(&state, &root, BACK_FLAG),
            "flag resets after firing",
        );

        let _ = state.put(&root, FORWARD_FLAG, true);
        w.sync(&mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));

        let _ = state.put(&root, RELOAD_FLAG, true);
        w.sync(&mut state, &root);
        assert!(
            engine.commands().contains(&WebViewCommand::Reload),
            "reload flag reaches the engine",
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

        let _ = state.put(&root, PIP_FLAG, true);
        w.sync(&mut state, &root); // sends the asynchronous probe
        assert!(
            engine
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::RequestMediaElement { .. }))
        );
        assert!(w.take_floating_surface_request().is_none());

        w.sync(&mut state, &root); // accepts the deterministic response
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
        w.apply_engine_events(&mut state, &root);
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

        let _ = state.put(&root, PIP_FLAG, true);
        w.sync(&mut state, &root);
        w.sync(&mut state, &root);

        assert!(w.take_floating_surface_request().is_none());
        assert_eq!(
            w.pip_feedback.as_ref().map(|feedback| feedback.kind),
            Some(PipFeedbackKind::NoVideo)
        );
    }

    #[test]
    fn no_video_select_area_requests_the_shared_shell_pip_picker_once() {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let (mut state, root) = synced(&mut w);
        w.pip_feedback = Some(PipFeedback {
            kind: PipFeedbackKind::NoVideo,
            started: std::time::Instant::now(),
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
                - PIP_FEEDBACK_LIFETIME
                - std::time::Duration::from_secs(1),
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
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
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
        w.apply_engine_events(&mut state, &root);

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

        engine.send(WebViewCommand::Navigate("https://qos.example".into()));
        w.sync(&mut state, &root);

        assert_eq!(
            BrowserWidget::read_url(&state, &root).as_deref(),
            Some("https://qos.example")
        );
        assert_eq!(w.app_title(), "https://qos.example — test page");
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
        let back = page.actions.iter().find(|a| a.id == "back").unwrap();
        assert!(!back.contract.availability.is_available());
        let navigate = page.actions.iter().find(|a| a.id == "navigate").unwrap();
        assert_eq!(navigate.params()[0].name.as_ref(), "url");
        assert_eq!(
            navigate.contract.execution,
            ui::qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(navigate.contract.result, ui::qos_actions::ResultSpec::None);
        let mut args = serde_json::Map::new();
        args.insert("url".into(), serde_json::json!("qos.example"));
        let encoded = navigate.build_json(&args).unwrap();
        let invocation = ui::qos_actions::InvocationId::new();
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
        assert_eq!(engine.current_uri().as_deref(), Some("https://qos.example"));
        w.sync(&mut doc, &root);
        assert!(matches!(
            w.poll_resource_operation(invocation, false, &doc, &root),
            Some(ui::qos_actions::InvocationUpdate::Succeeded { .. })
        ));
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
}
