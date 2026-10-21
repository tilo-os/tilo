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

use automerge::{AutoCommit, ObjId};
use std::rc::Rc;
use taffy::{NodeId, Size, Style, TaffyTree};
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{ActionBinding, ControlInvocation, Resource};
use ui::webview::test_engine::TestEngine;
use ui::webview::{
    MediaElementInfo, NavigationCommand, NavigationDirection, PageFailure, PageReply, PageRequest,
    SharedEngine, WebViewCommand, WebViewEvent,
};
use ui::widget::{
    AppKind, AppWidget, Button, Column, Container, ContextMenu, Continuable, EmptyState,
    FloatingSurfaceProvider, IconDescriptor, MenuItem, Popover, WebView, Widget,
};

use crate::actions::history_resource_id;
use crate::address_overlay::AddressSuggestions;
use crate::browsing_mode::BrowsingMode;
use crate::downloads::{DownloadStatus, Downloads};
use crate::history::History;
use crate::library::Bookmarks;
use crate::omnibox;
use crate::resource::{Browser, bookmark_resource_id, browser};
use crate::settings::{Settings, SettingsActionBindings, SettingsPane, SettingsPresentation};
use ui::lock::MutexRecover as _;
use ui::tilo_resource::Invoke;
use ui::tilo_resource::Resource as _;

mod continuity;
mod library;
pub(crate) mod personal;
mod toolbar;
mod window_bar;

const PIP_ID: &str = "browser.media-pip";
const PIP_ELEMENT_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// e2e id of the address input (also used internally to detect whether
/// the user is editing it).
const URL_INPUT_ID: &str = "browser.url_input";
const PIP_FEEDBACK_LIFETIME: std::time::Duration = std::time::Duration::from_millis(4200);
const SEARCH_QUERY_TOKEN: &str = "{query}";
/// A `WebView` backend is external work: it can fail to produce either a load
/// completion or an explicit failure.  Never leave an accepted resource
/// invocation pending forever in that case.
const NAVIGATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// Media discovery is asynchronous.  Keep a resource invocation pending only
/// until the engine gives its positive/negative answer, never indefinitely.
const PIP_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A page action on a loading page waits this long for it to settle, then
/// acts on the page as it stands.
const PAGE_LOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
/// The engine's answer to one page request never stays pending longer.
const PAGE_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// A page action that started a navigation waits this long for it to load.
const PAGE_NAVIGATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A page action admitted through the resource surface.
struct PageOperation {
    invocation: ui::tilo_actions::InvocationId,
    request_id: u32,
    phase: PagePhase,
    deadline: std::time::Instant,
    /// A main-frame load that settled while the request was with the engine
    /// and how. The engine answers a navigating action only once its page
    /// loaded, after the tab heard so: an answer that says "loading" then
    /// completes at once instead of waiting for a load that already happened.
    settled_while_sent: Option<SettledLoad>,
}

/// How a main-frame load settled.
enum SettledLoad {
    Loaded,
    Failed(String),
}

enum PagePhase {
    /// The page is loading; the request goes to the engine once it settles.
    Queued {
        request: PageRequest,
        until: std::time::Instant,
    },
    /// Sent to the engine, which answers once.
    Sent,
    /// The action started a navigation; it completes when that settles.
    Navigating { reply: PageReply },
}

fn decode_favicon_png(
    bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
) -> Option<vello::peniko::ImageData> {
    let decoded = ui::png::decode_rgba8_within(bytes, 64 * 64).ok()?;
    if decoded.width != expected_width || decoded.height != expected_height {
        return None;
    }
    Some(ui::raster::rgba8(
        decoded.rgba,
        expected_width,
        expected_height,
    ))
}

/// Browser's public navigation boundary. Keep this separate from the generic
/// embedded-view helper: Browser's manifest advertises only these schemes.
fn is_supported_web_url(value: &str) -> bool {
    url::Url::parse(value.trim())
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

/// The page that searches `query` with the engine `template` (a URL that
/// takes the query after it, or holds `{query}`).
fn search_url(template: &str, query: &str) -> String {
    let query: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    if template.contains(SEARCH_QUERY_TOKEN) {
        template.replace(SEARCH_QUERY_TOKEN, &query)
    } else {
        format!("{template}{query}")
    }
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
    started: ui::time::Instant,
    /// Screenshot/debug guidance is deliberately steady: headless rendering
    /// can be queued behind a busy renderer, and must not race the ordinary
    /// short-lived live note away before it is inspected.
    persistent: bool,
}

/// What a person chose in the no-video note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipChoice {
    FloatPage,
    SelectArea,
}

/// The ids of the toolbar controls a panel opens beside.
fn anchor_ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// The panel anchored to a control of the toolbar, so a dismissal can say
/// which one closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelKind {
    Credentials,
    Site,
    Prompt,
    Pip,
    Extension,
}

impl PipFeedback {
    fn transient(kind: PipFeedbackKind) -> Self {
        Self {
            kind,
            started: ui::time::now(),
            persistent: false,
        }
    }

    fn persistent(kind: PipFeedbackKind) -> Self {
        Self {
            kind,
            started: ui::time::now(),
            persistent: true,
        }
    }

    fn is_expired(&self) -> bool {
        !self.persistent
            && ui::time::now().saturating_duration_since(self.started) >= PIP_FEEDBACK_LIFETIME
    }
}

pub(crate) use crate::effects::{BrowserOperation, LibrarySection};

const MAX_LIBRARY_FILTER_BYTES: usize = 256;

pub(crate) use browser_parts::rows::library_filter_matches;

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

fn local_output(message: impl Into<String>) -> ui::tilo_actions::ActionOutput {
    ui::tilo_actions::ActionOutput {
        value: Some(serde_json::Value::String(message.into())),
        resources: Vec::new(),
    }
}

fn download_status_text(entry: &crate::downloads::DownloadEntry) -> String {
    match entry.status {
        DownloadStatus::AwaitingDestination => "Waiting for a save destination…".into(),
        DownloadStatus::Downloading => match entry.total_bytes {
            Some(total) => format!(
                "{} of {}",
                ui::format::size(entry.received_bytes),
                ui::format::size(total)
            ),
            None => format!("{} downloaded", ui::format::size(entry.received_bytes)),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingHistory {
    request_id: u32,
    direction: NavigationDirection,
    document_revision: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompositionView {
    Page { find: bool },
    Settings,
    Library(LibrarySection),
}

#[derive(Clone, PartialEq)]
struct CompositionKey {
    theme: Theme,
    view: CompositionView,
    /// The window's bar shows the browser's slots, so it draws no toolbar.
    hosted: bool,
    compact: bool,
    immersive: bool,
    window_scale: f32,
}

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
    service_selections: Vec<crate::service_sessions::Selection>,
    service_destination: Option<crate::service_destinations::Destination>,
    service_redirect: Option<String>,
    service_client: Option<ui::collaboration::browser_service_sessions::Client>,
    service_catalog_pending: Option<ui::collaboration::browser_service_sessions::Pending>,
    service_list_reply: Option<ui::framework::EffectReply>,
    service_list_waker: Option<ui::WakeHandle>,
    service_list_pending: Option<ui::collaboration::browser_service_sessions::Pending>,
    service_observation: Option<serde_json::Value>,
    service_pending: Option<crate::service_sessions::PendingView>,
    service_view: Option<crate::service_sessions::ServiceView>,
    service_forget: Option<crate::service_sessions::PendingForget>,
    browsing_mode: BrowsingMode,
    composed: ui::widget::Composed,
    composition_key: Option<CompositionKey>,
    composition_dirty: bool,
    bootstrap_done: bool,
    /// `--shots` mode: no network, no filter loader, deterministic page.
    headless: bool,
    /// The resource's dispatcher: bookmarks and preferences are verbs and
    /// field edits submitted through it. `None` until a host attaches
    /// one; a bare widget changes no document.
    dispatch: Option<ui::framework::Dispatcher<crate::resource::Browser>>,
    /// The origins tracker protection is off for, as the document holds them.
    site_exceptions: std::collections::BTreeSet<String>,
    initial_url: String,
    /// What a launch asked to search the web for (desktop search's "Search
    /// the web for …"): the first page is its results on the person's own
    /// search engine, which may arrive with the document after the window
    /// is built.
    initial_search: Option<String>,
    /// An address draft belongs to this window. Only an explicit Navigate
    /// resource operation crosses the browser/engine boundary.
    address: ui::LocalState<String>,
    /// Draft value at an explicit navigation; newer edits remain local.
    address_navigation_value: Option<String>,
    daily: crate::daily_controls::DailyControls,
    page_menu: crate::page_menu::PageMenu,
    site: crate::site_controls::SiteControls,
    credentials: crate::credentials::Credentials,
    /// The popover open next to the toolbar control it belongs to: the site
    /// or autofill panel, a permission or download question, or the no-video
    /// note.
    anchored_panel: Option<Box<dyn Widget>>,
    panel_dismissed: ui::LocalState<Option<PanelKind>>,
    pip_choice: ui::LocalState<Option<PipChoice>>,
    pending_chrome_focus: Option<(NodeId, &'static str)>,
    private_window_request: ui::LocalState<bool>,
    private_window_notice: Option<String>,
    window_scale: f32,
    native_closed: bool,
    close_blocked: bool,

    // Chrome state mirrored from engine events.
    can_go_back: bool,
    loading_stopped: bool,
    can_go_forward: bool,
    /// `1.0` when idle; anything below shows the loading line.
    progress: f64,
    /// When the current load began, the loading line's sweep epoch.
    loading_started: Option<ui::time::Instant>,
    /// The page shows its article in reader view.
    reader_view: bool,
    /// The reader view a running page operation asked for.
    reader_request: Option<bool>,
    title: String,
    favicon: Option<vello::peniko::ImageData>,
    /// The favicon as the engine sent it, for the window's bar
    /// (`ui::window_toolbar::ToolbarImage`).
    favicon_png: Option<Vec<u8>>,
    /// What the person is typing into the address in the window's bar, as
    /// the bar last said (`ToolbarEvent::Edit`); `None` while it is not
    /// being edited there.
    bar_query: Option<String>,
    audio_muted: bool,
    media_playing: bool,
    media_document_generation: Option<u64>,
    /// Most recent URI the engine reported — recorded into history on
    /// load-finished.
    current_uri: String,
    /// What the window's bar slots were last declared from
    /// (`window_bar::BarInputs`), so an unchanged frame declares nothing.
    declared_bar: Option<window_bar::BarInputs>,
    /// The press that selected the whole address: its release is the
    /// address bar's too, so the selection it made stays.
    swallow_address_release: bool,
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
    capabilities: crate::capabilities::Capabilities,
    /// Extensions: the Web Store offer, toolbar actions, popups and the
    /// Library manager.
    extensions: crate::extension_controller::ExtensionController,
    settings_presentation: SettingsPresentation,
    /// History pausing is an in-memory, device-local session boundary. It is
    /// never serialized to the collaborative document or the profile.
    history_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    observed_history_paused: bool,
    /// Local presentation error after a history-clear write failed. Kept in
    /// the browser (rather than the shared document) because it describes
    /// this profile's disk, not collaborative state.
    history_clear_error: Option<String>,
    /// The person's browsing history, which this window records its visits
    /// in; `None` keeps them nowhere (a private window, a screenshot).
    person_bookmarks:
        Option<ui::framework::PersonRecord<crate::personal_bookmarks::PersonalBookmarks>>,
    personal_bookmarks: Rc<crate::personal_bookmarks::PersonalBookmarkState>,
    personal_bookmarks_revision: Option<u64>,
    personal_open: ui::LocalState<std::collections::BTreeMap<String, String>>,
    personal_bookmark_location: crate::personal_bookmark_view::Location,
    person_history: Option<ui::framework::PersonRecord<crate::history::BrowsingHistory>>,
    continuity: Option<crate::continuity_handoff::Controller>,
    /// The person's address book, which new tabs fill forms from too.
    address_book: Option<ui::framework::PersonRecord<crate::autofill::AddressBook>>,
    /// The record's revision [`Self::history`] shows.
    history_revision: Option<u64>,
    history: History,
    bookmarks: Bookmarks,
    downloads: Downloads,
    download_picker: Option<(String, ui::portal::PortalChannel)>,
    download_picker_queue: std::collections::VecDeque<(String, String)>,
    download_save_due: Option<ui::time::Instant>,
    /// Retained full-corpus index. Provider handles are cheap and unchanged
    /// frames compare only the durable corpus revision tokens.
    search_index: crate::search_provider::BrowserSearchIndex,
    /// Fuzzy suggestion dropdown under the address bar (a field, not a
    /// tree child: driven here and painted in `draw_overlay`).
    suggestions: AddressSuggestions,
    navigation_hold: crate::back_hold::NavigationHold,
    navigation_pointer_down: Option<NavigationDirection>,
    back_menu: ContextMenu,
    back_entries: Vec<ui::webview::NavigationEntry>,
    menu_direction: Option<NavigationDirection>,
    history_request: u32,
    pending_history: Option<PendingHistory>,
    document_revision: u64,
    /// Whether the settings pane replaces the page surface.
    show_settings: bool,
    show_library: bool,
    library_section: LibrarySection,
    library_filter: ui::LocalState<String>,
    /// Set when `show_settings` flips, so the host rebuilds the tree.
    structure_dirty: bool,
    /// Last address-bar text a suggestion query ran for, to avoid
    /// re-querying every frame.
    last_query: Option<String>,
    /// Visited pages prepared for per-keystroke matching.
    omni_index: omnibox::HistoryIndex,
    /// The index must be rebuilt: the history changed since it was built.
    omni_stale: bool,
    /// Quiet-period and supersede bookkeeping for the engine's suggestions.
    suggest_debounce: omnibox::Debounce,
    /// Fetches of the engine's suggestions, answering into `remote`.
    suggest_fetcher: omnibox::SuggestFetcher,
    /// The engine's suggestions for the text being typed.
    remote: Vec<String>,
    /// The text last typed by the person (not the inline completion).
    last_typed: String,
    /// An inline completion shown in the address field: `(typed, full)`.
    inline: Option<(String, String)>,
    /// Select the completed suffix of the address field on the next sync.
    select_completion: Option<(usize, usize)>,
    /// Debug/`--shots-overlay`: keep forced suggestions visible without a
    /// real focus + query, so the overlay can be rendered headlessly.
    force_suggestions: bool,
    /// The `WebView` content rect as window fractions `[x0,y0,x1,y1]`,
    /// published each layout for the zero-copy compositor to blit into.
    content_rect: std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>>,
    resource_navigation_invocation: Option<ui::tilo_actions::InvocationId>,
    resource_navigation_request: Option<u32>,
    next_navigation_request: u32,
    explicit_navigation_requested: bool,
    resource_navigation_result: Option<ui::tilo_actions::InvocationUpdate>,
    resource_navigation_deadline: Option<ui::time::Instant>,
    resource_pip_invocation: Option<ui::tilo_actions::InvocationId>,
    resource_pip_request: Option<u64>,
    resource_pip_result: Option<ui::tilo_actions::InvocationUpdate>,
    resource_pip_deadline: Option<ui::time::Instant>,
    page_operation: Option<PageOperation>,
    page_operation_result: Option<ui::tilo_actions::InvocationUpdate>,
    /// Effects waiting on an operation this window accepted.
    pending_replies: Vec<(ui::tilo_actions::InvocationId, ui::framework::EffectReply)>,
    page_request_seq: u32,
    pending_pip: Option<ui::floating_surface::FloatingSurfaceRequest>,
    pending_pip_region_update: Option<Rect>,
    published_pip_region: Option<Rect>,
    /// CEF document/video fullscreen state for this tab.
    content_fullscreen: bool,
    /// Actual primary-window state reported by the root widget.
    window_fullscreen_active: bool,
    active_pip_element: Option<MediaElementInfo>,
    /// The no-video fallback mirrors the live page viewport instead of a
    /// semantic media element. Kept explicit so media polling never mistakes
    /// it for a disappeared `<video>` and closes it a quarter-second later.
    pip_page_fallback: bool,
    active_pip: bool,
    pip_id: String,
    pip_request_seq: u64,
    active_pip_probe: Option<u64>,
    active_pip_refresh_probe: Option<u64>,
    next_pip_refresh_at: Option<ui::time::Instant>,
    pip_feedback: Option<PipFeedback>,
    pending_capture_studio: Option<ui::widget::CaptureMode>,
    cursor: Point,
    /// Whether the window is narrow enough for the compact toolbar; see
    /// [`is_compact_chrome`].
    compact_chrome: bool,
    /// The theme's compact breakpoint at the last layout pass.
    compact_breakpoint: f32,
    /// The theme's text scale at the last layout pass. The compact tier is
    /// judged on the width the text leaves, so a 1200px window at 2× lays
    /// out as the 600px window it is to the eye.
    text_scale: f32,
    /// A panel a review capture stages. The first page load closes every
    /// panel, so the fixture waits until the load events have been applied.
    review_stage: Option<ReviewStage>,
    /// Resource actions that menu rows and row actions queued; drained in
    /// `sync` (see `toolbar.rs`).
    pending_commands: ui::LocalState<Vec<toolbar::ResourceCommand>>,
    motion_reduced: bool,
    pending_controls: std::collections::VecDeque<ControlInvocation>,
    /// Set when live browsing has no running engine; the page area explains
    /// it instead of showing a blank page.
    engine_problem: Option<EngineProblem>,
    /// The engine being fetched from the host, when it is missing.
    engine_setup: Option<crate::engine_setup::EngineSetup>,
    /// Whether the first use has already asked for the engine.
    engine_setup_tried: bool,
    /// What the page area last drew of the setup.
    engine_setup_shown: Option<crate::engine_setup::Setup>,
    /// Where the host is asked for the engine; a test supplies its own.
    engine_installer: fn() -> Option<Box<dyn ui::webview::chromium::install::EngineInstaller>>,
}

impl BrowserWidget {
    /// Live-mode widget on the Chromium engine runtime. Without the runtime
    /// the page area offers to install it.
    pub fn new_with_mode(mode: BrowsingMode) -> Self {
        let (engine, problem) = default_engine(mode);
        let mut widget = Self::with_engine_mode(engine, false, mode);
        widget.engine_problem = problem;
        widget
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

    pub(crate) fn attach_service_waker(&mut self, waker: ui::WakeHandle) {
        {
            if let Some(view) = self.service_view.as_mut() {
                view.attach_waker(waker.clone());
            }
            if let Some(view) = self.service_pending.as_mut() {
                view.attach_waker(waker);
            }
        }
    }
    pub(crate) fn begin_forget_service_session(
        &mut self,
        id: &str,
        person: [u8; 32],
        reply: ui::framework::EffectReply,
        waker: ui::WakeHandle,
    ) -> Result<(), String> {
        if !crate::service_profiles::supported() {
            return Err("Local account forgetting needs the native backend".into());
        }
        if self.service_forget.is_some()
            || self.service_pending.is_some()
            || self.service_catalog_pending.is_some()
            || self.service_list_pending.is_some()
        {
            return Err("Account operation is still pending; retry when it settles".into());
        }
        let selection = self
            .service_selection(id)
            .cloned()
            .ok_or("This account needs to be connected again")?;
        // Clone the originally admitted route before handing work off-thread.
        let client = self
            .service_client
            .clone()
            .ok_or("Account custody is unavailable")?;
        self.service_forget = Some(crate::service_sessions::PendingForget::start(
            selection,
            person,
            client,
            self.service_view.as_mut(),
            reply,
            waker,
        )?);
        self.structure_dirty = true;
        Ok(())
    }

    pub(crate) fn request_service_close(&mut self) -> bool {
        {
            if let Some(view) = self.service_pending.as_mut() {
                return view.request_close();
            }
            if let Some(view) = self.service_view.as_mut() {
                return view.request_close();
            }
        }
        true
    }
    pub(crate) fn service_close_complete(&self) -> bool {
        {
            if let Some(view) = self.service_pending.as_ref() {
                return view.close_complete();
            }
            if let Some(view) = self.service_view.as_ref() {
                return view.close_complete();
            }
        }
        true
    }
    pub(crate) fn finish_service_close(&mut self) -> Result<(), String> {
        {
            if let Some(view) = self.service_pending.as_mut() {
                return view.finish_close();
            }
            if let Some(view) = self.service_view.as_mut() {
                return view.finish_close();
            }
        }
        Ok(())
    }

    pub(crate) fn with_service_client(
        mut self,
        client: Option<ui::collaboration::browser_service_sessions::Client>,
    ) -> Self {
        self.service_client = client;
        self
    }
    pub(crate) fn begin_service_session(
        &mut self,
        id: &str,
        person: [u8; 32],
        waker: ui::WakeHandle,
    ) -> Result<(), String> {
        if !crate::service_profiles::supported() {
            return Err("Local account opening needs the native backend".into());
        }
        if self.service_forget.is_some()
            || self.service_pending.is_some()
            || self.service_view.is_some()
            || self.service_list_pending.is_some()
        {
            return Err("An account is already opening or open in this window".into());
        }
        let destination = self.service_destination.clone().filter(|value| {
            self.service_selection(id)
                .is_some_and(|selected| value.service == selected.service)
        });
        self.service_observation = None;
        let anchor = crate::service_destinations::anchor(id, destination.as_ref());
        let client = self
            .service_client
            .clone()
            .ok_or("This account needs to be connected again")?;
        let mut pending = crate::service_sessions::PendingView::new(id.to_owned(), person, client)
            .with_destination(destination);
        pending.attach_waker(waker);
        pending.opening.set(Some(anchor));
        self.install_pending_service(pending);
        Ok(())
    }

    pub(crate) fn service_selection(
        &self,
        id: &str,
    ) -> Option<&crate::service_sessions::Selection> {
        self.service_selections.iter().find(|value| value.id == id)
    }
    pub(crate) fn begin_service_destination(
        &mut self,
        destination: crate::service_destinations::Destination,
    ) {
        self.service_destination = Some(destination);
        self.explicit_navigation_requested = true;
        self.initial_url = "about:blank".into();
        self.structure_dirty = true;
    }
    pub(crate) fn service_open_anchor(
        &self,
        selected: &crate::service_sessions::Selection,
    ) -> String {
        crate::service_destinations::anchor(
            &selected.id,
            self.service_destination
                .as_ref()
                .filter(|value| value.service == selected.service),
        )
    }
    pub(crate) fn take_service_redirect(&mut self) -> Option<String> {
        self.service_redirect.take()
    }
    pub(crate) fn accept_service_catalog(
        &mut self,
        sessions: Vec<ui::collaboration::browser_service_sessions::BrowserServiceSessionDescriptor>,
    ) {
        if !(ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::List {
            sessions: sessions.clone(),
        })
        .valid()
        {
            return;
        }
        self.service_selections = sessions
            .into_iter()
            .map(crate::service_sessions::selection)
            .collect();
        if let Some(destination) = &self.service_destination {
            let matching: Vec<_> = self
                .service_selections
                .iter()
                .filter(|value| value.service == destination.service)
                .collect();
            match matching.as_slice() {
                [selected] => self.service_redirect = Some(self.service_open_anchor(selected)),
                [] => {
                    // No approved session exists: ordinary sign-in remains available.
                    self.engine
                        .send(WebViewCommand::Navigate(destination.uri.clone()));
                    self.service_destination = None;
                }
                _ => {} // Existing selected-account actions require an explicit choice.
            }
        }
        self.structure_dirty = true;
    }
    pub(crate) fn attach_service_catalog(&mut self, waker: ui::WakeHandle) {
        if let Some(client) = &self.service_client {
            self.service_catalog_pending = client
                .request(
                    ui::collaboration::browser_service_sessions::BrowserServiceSessionCommand::List,
                    Some(waker),
                )
                .ok();
            if self.service_catalog_pending.is_none() {
                self.accept_service_catalog(Vec::new());
            }
        }
    }
    /// Install the pending native credential request selected by the canonical
    /// account action, before the polling pass resolves it into a fresh engine.
    fn install_pending_service(&mut self, pending: crate::service_sessions::PendingView) {
        self.explicit_navigation_requested = true;
        self.service_pending = Some(pending);
        self.structure_dirty = true;
    }
    pub(crate) fn begin_service_list(
        &mut self,
        reply: ui::framework::EffectReply,
        waker: ui::WakeHandle,
    ) -> Result<(), String> {
        if self.service_client.is_none() || self.service_list_reply.is_some() {
            return Err("Account custody is unavailable or busy".into());
        }
        self.service_list_reply = Some(reply);
        self.service_list_waker = Some(waker);
        self.poll_service_list();
        Ok(())
    }

    fn poll_service_list(&mut self) {
        use ui::collaboration::browser_service_sessions::{
            BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
        };
        if self.service_list_reply.is_none() {
            return;
        }
        if self.service_list_pending.is_none() {
            if self.service_catalog_pending.is_some() {
                return;
            }
            if self.service_pending.is_some() || self.service_forget.is_some() {
                return;
            }
            let result = self
                .service_client
                .as_ref()
                .ok_or_else(|| "Account inventory could not be confirmed".to_owned())
                .and_then(|client| client.request(Command::List, self.service_list_waker.clone()));
            match result {
                Ok(pending) => self.service_list_pending = Some(pending),
                Err(error) => {
                    if let Some(reply) = self.service_list_reply.take() {
                        reply.finish(Err(error));
                    }
                    return;
                }
            }
        }
        let Some(result) = self
            .service_list_pending
            .as_mut()
            .and_then(ui::collaboration::browser_service_sessions::Pending::poll)
        else {
            return;
        };
        self.service_list_pending = None;
        let result = result.and_then(|reply| match reply {
            Reply::List { sessions } => {
                let value = serde_json::json!({"schema": 1, "sessions": sessions.iter().map(|row| serde_json::json!({"id": row.id, "service": row.service})).collect::<Vec<_>>()});
                self.accept_service_catalog(sessions);
                Ok(ui::tilo_actions::ActionOutput { value: Some(value), ..Default::default() })
            }
            _ => Err("Account inventory could not be confirmed".into()),
        });
        if let Some(reply) = self.service_list_reply.take() {
            reply.finish(result);
        }
    }

    fn poll_service_admission(&mut self) {
        if self
            .service_forget
            .as_mut()
            .is_some_and(|pending| pending.poll(self.service_view.as_mut()))
        {
            self.service_observation = self
                .service_forget
                .as_ref()
                .map(crate::service_sessions::PendingForget::report);
            if let Some(report) = &self.service_observation
                && report["custody_revoked"] == true
            {
                self.service_selections
                    .retain(|selected| Some(selected.id.as_str()) != report["session_id"].as_str());
            }
            self.service_forget = None;
            self.structure_dirty = true;
        }
        if let Some(reply) = self
            .service_catalog_pending
            .as_mut()
            .and_then(ui::collaboration::browser_service_sessions::Pending::poll)
        {
            self.service_catalog_pending = None;
            if let Ok(
                ui::collaboration::browser_service_sessions::BrowserServiceSessionReply::List {
                    sessions,
                },
            ) = reply
            {
                self.accept_service_catalog(sessions);
            } else {
                self.accept_service_catalog(Vec::new());
            }
        }
        if let Some(reply) = self
            .service_catalog_pending
            .is_none()
            .then(|| {
                self.service_pending
                    .as_mut()
                    .and_then(|pending| pending.poll(&self.service_selections))
            })
            .flatten()
        {
            self.service_observation = self.service_pending.as_ref().map(|pending| {
                let mut report = pending.report();
                if let Some(object) = report.as_object_mut() {
                    object.insert("status".into(), serde_json::json!("needs-reconnect"));
                    object.insert("requires_reconnect".into(), serde_json::json!(true));
                }
                report
            });
            self.service_pending = None;
            match reply {
                Ok(mut view) => {
                    self.service_observation = None;
                    self.engine = view.engine();
                    view.opening.set(Some(view.anchor()));
                    let _ = view.poll_open();
                    self.service_view = Some(view);
                    self.bootstrap_done = false;
                }
                Err(error) => self.engine = ui::webview::chromium::UnavailableEngine::new(error),
            }
            self.structure_dirty = true;
        }
        self.poll_service_list();
    }

    #[cfg(test)]
    pub(crate) fn with_service_view(mut self, view: crate::service_sessions::ServiceView) -> Self {
        self.explicit_navigation_requested = true;
        self.service_view = Some(view);
        self
    }

    /// Start on the results for `query` on the person's search engine, as a
    /// launch from desktop search asks (`web+search:`).
    #[must_use]
    pub fn with_initial_search(mut self, query: String) -> Self {
        self.initial_url = search_url(&self.settings.search_engine, &query);
        self.address.set(self.initial_url.clone());
        self.initial_search = Some(query);
        self
    }

    /// Set the first URI loaded before the widget bootstraps. The binary uses
    /// this only after validating the URI against Browser's declared schemes.
    #[must_use]
    pub fn with_initial_uri(mut self, uri: String) -> Self {
        if !self.browsing_mode.is_private()
            && self.service_client.is_some()
            && let Some(destination) = crate::service_destinations::Destination::parse(&uri)
        {
            self.begin_service_destination(destination);
        } else {
            self.initial_url = uri;
        }
        self
    }

    /// The window width the responsive tiers are judged on: the real width
    /// in 1× pixels, so a 1200px window at a 2× text scale lays out as the
    /// 600px window it is to the eye rather than starving its address field.
    fn chrome_width(&self) -> f64 {
        self.rect.width() / f64::from(self.text_scale.max(1.0))
    }

    /// Handle to the engine this widget drives — the binary uses it to
    /// wire the engine's waker to the winit event-loop proxy.
    pub fn engine_handle(&self) -> SharedEngine {
        self.engine.clone()
    }
    #[cfg(test)]
    pub(crate) fn observed_page_uri(&self) -> &str {
        &self.current_uri
    }

    pub(crate) fn content_fullscreen(&self) -> bool {
        self.content_fullscreen
    }
    #[cfg(test)]
    pub(crate) fn force_content_fullscreen(&mut self, active: bool) {
        self.content_fullscreen = active;
        self.structure_dirty = true;
    }
    pub(crate) fn set_window_fullscreen_active(&mut self, active: bool) {
        if self.window_fullscreen_active != active {
            self.window_fullscreen_active = active;
            self.structure_dirty = true;
        }
    }
    /// Clear this tab's desired state immediately and tell Alloy to leave
    /// document fullscreen. Returns whether an exit was initiated.
    pub(crate) fn exit_content_fullscreen(&mut self) -> bool {
        if !self.content_fullscreen {
            return false;
        }
        self.content_fullscreen = false;
        self.structure_dirty = true;
        self.engine.send(WebViewCommand::ExitFullscreen);
        true
    }

    pub(crate) fn deactivate_credentials(&mut self) {
        let was_open = self.credentials.open;
        let requests = self.credentials.close();
        crate::credentials::Credentials::send(&self.engine, requests);
        self.structure_dirty |= was_open;
    }

    pub(crate) fn request_private_window(&self) {
        self.private_window_request.set(true);
    }
    /// Put the caret in the address field, for a window opened without a
    /// page to show.
    pub(crate) fn focus_address_with_caret(&mut self) -> bool {
        if ui::window_toolbar::is_hosted() {
            // The address is the window bar's location: the caret waits
            // there.
            ui::window_toolbar::edit_location();
            return false;
        }
        self.focus_address_input()
    }
    /// Show `url` in another Browser in a desktop tab right after this one.
    /// A private page never hands its URL to a normal launch: the new page
    /// is another private window, placed as the tab beside.
    pub(crate) fn open_beside(&mut self, url: String, background: bool) {
        if self.browsing_mode.is_private() {
            ui::shell_tabs::place_next_window_beside(background);
            if let Err(error) = crate::private_window::launch(Some(&url)) {
                self.private_window_notice = Some(error);
                self.structure_dirty = true;
            }
        } else {
            ui::shell_tabs::open_beside(Some(url), background);
        }
    }
    pub(crate) fn history_paused(&self) -> bool {
        self.history_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn set_window_scale(&mut self, scale: f64) {
        self.window_scale = scale as f32;
    }
    pub(crate) fn take_native_closed(&mut self) -> bool {
        std::mem::take(&mut self.native_closed)
    }
    pub(crate) fn download_exit_requires_choice(&self) -> bool {
        self.capabilities.exit_requires_choice()
    }
    pub(crate) fn download_exit_cleanup_complete(&self) -> bool {
        self.capabilities.exit_cleanup_complete()
    }
    pub(crate) fn download_exit_status(&self) -> Option<String> {
        self.capabilities.toolbar_status()
    }
    pub(crate) fn cancel_downloads_for_exit(&self) {
        crate::capabilities::Capabilities::cancel_all(&self.engine);
    }
    /// Keep this window's visits in the person's browsing history, and show
    /// it.
    /// Keep the download list in this device's settings, starting from the
    /// list kept there.
    pub(crate) fn attach_downloads(
        &mut self,
        store: ui::storage::Settings<crate::downloads::Stored>,
    ) {
        self.downloads = Downloads::load(store);
        self.structure_dirty = true;
    }

    pub(crate) fn attach_history(
        &mut self,
        record: ui::framework::PersonRecord<crate::history::BrowsingHistory>,
    ) {
        self.person_history = Some(record);
        self.history_revision = None;
    }

    pub(crate) fn personal_open_handle(
        &self,
    ) -> ui::LocalState<std::collections::BTreeMap<String, String>> {
        self.personal_open.clone()
    }

    pub(crate) fn attach_personal_bookmarks(
        &mut self,
        record: ui::framework::PersonRecord<crate::personal_bookmarks::PersonalBookmarks>,
    ) {
        self.person_bookmarks = Some(record);
        self.personal_bookmarks_revision = None;
    }
    fn follow_personal_bookmarks(&mut self) {
        let Some(record) = &self.person_bookmarks else {
            return;
        };
        let revision = record.revision();
        if self.personal_bookmarks_revision != Some(revision) {
            self.personal_bookmarks_revision = Some(revision);
            self.personal_bookmarks = Rc::new(record.get().projected());
            let mut opened = std::collections::BTreeMap::new();
            for entry in self.personal_bookmarks.bookmarks.entries() {
                opened.insert(
                    crate::personal_bookmarks::flat_id(&entry.url),
                    entry.url.clone(),
                );
            }
            for tree in &self.personal_bookmarks.imported_trees {
                for node in &tree.nodes {
                    if let crate::imported_folders::NodeKind::Bookmark { url } = &node.kind {
                        opened.insert(
                            crate::personal_bookmarks::node_id(&tree.source, &node.id),
                            url.clone(),
                        );
                    }
                }
            }
            self.personal_open.set(opened);
            self.structure_dirty = true;
        }
    }
    /// Fill forms from, and edit, the person's address book.
    pub(crate) fn attach_address_book(
        &mut self,
        book: ui::framework::PersonRecord<crate::autofill::AddressBook>,
    ) {
        self.address_book = Some(book.clone());
        self.credentials.attach_address_book(book);
    }

    /// Show the person's browsing history again when it changed: a visit
    /// in another tab, window or device, a clear.
    fn follow_history(&mut self) {
        let Some(record) = &self.person_history else {
            return;
        };
        let revision = record.revision();
        if self.history_revision != Some(revision) {
            self.history_revision = Some(revision);
            self.history = crate::history::History::of(&record.get());
            self.omni_stale = true;
            self.last_query = None;
            self.search_index.invalidate();
            self.structure_dirty = true;
        }
    }

    pub fn with_engine(engine: SharedEngine, headless: bool) -> Self {
        Self::with_engine_mode(engine, headless, BrowsingMode::Normal)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one explicit initializer keeps all browser state defaults visible together"
    )]
    pub fn with_engine_mode(
        engine: SharedEngine,
        headless: bool,
        browsing_mode: BrowsingMode,
    ) -> Self {
        // Device-local state; `load()` returns empty when absent, so
        // `--shots` runs are unaffected by (and don't write) real files.
        // Preferences and bookmarks are the document's, presented later.
        let persistent = !headless && browsing_mode.persists_personal_state();
        let settings = Settings::default();
        let history = History::default();
        let bookmarks = Bookmarks::default();
        let downloads = Downloads::default();
        let initial_url = if is_supported_web_url(&settings.homepage) {
            settings.homepage.clone()
        } else {
            DEFAULT_URL.into()
        };
        let settings_presentation = SettingsPresentation::from_settings(&settings);
        let search_index = crate::search_provider::BrowserSearchIndex::default();
        search_index.sync_all(&history, &downloads);
        Self {
            rect: Rect::ZERO,
            engine,
            browsing_mode,
            composed: ui::widget::Composed::default(),
            composition_key: None,
            composition_dirty: false,
            bootstrap_done: false,
            service_selections: Vec::new(),
            service_destination: None,
            service_redirect: None,
            service_client: None,
            service_catalog_pending: None,
            service_list_reply: None,
            service_list_waker: None,
            service_list_pending: None,
            service_observation: None,
            service_pending: None,
            service_view: None,
            service_forget: None,
            headless,
            dispatch: None,
            site_exceptions: std::collections::BTreeSet::new(),
            address: ui::LocalState::new(initial_url.clone()),
            address_navigation_value: None,
            daily: crate::daily_controls::DailyControls::default(),
            page_menu: crate::page_menu::PageMenu::default(),
            site: crate::site_controls::SiteControls::default(),
            credentials: crate::credentials::Credentials::new(browsing_mode.is_private()),
            anchored_panel: None,
            panel_dismissed: ui::LocalState::new(None),
            pip_choice: ui::LocalState::new(None),
            pending_chrome_focus: None,
            private_window_request: ui::LocalState::new(false),
            private_window_notice: None,
            window_scale: 1.0,
            native_closed: false,
            close_blocked: false,
            initial_url,
            initial_search: None,
            can_go_back: false,
            loading_stopped: false,
            can_go_forward: false,
            progress: if headless { 1.0 } else { 0.0 },
            loading_started: None,
            reader_view: false,
            reader_request: None,
            title: String::new(),
            favicon: None,
            favicon_png: None,
            bar_query: None,
            audio_muted: false,
            media_playing: false,
            media_document_generation: None,
            current_uri: String::new(),
            declared_bar: None,
            swallow_address_release: false,
            load_error: None,
            address_error_active: false,
            forced_load_error: false,
            forced_loading: false,
            settings,
            capabilities: crate::capabilities::Capabilities::default(),
            extensions: crate::extension_controller::ExtensionController::new(persistent),
            settings_presentation,
            history_paused: std::sync::Arc::default(),
            observed_history_paused: false,
            history_clear_error: None,
            person_bookmarks: None,
            personal_bookmarks: Rc::default(),
            personal_bookmarks_revision: None,
            personal_open: ui::LocalState::new(std::collections::BTreeMap::new()),
            personal_bookmark_location: crate::personal_bookmark_view::Location::default(),
            person_history: None,
            continuity: None,
            address_book: None,
            history_revision: None,
            history,
            bookmarks,
            downloads,
            download_picker: None,
            download_picker_queue: std::collections::VecDeque::new(),
            download_save_due: None,
            search_index,
            suggestions: AddressSuggestions::new(),
            navigation_hold: crate::back_hold::NavigationHold::default(),
            navigation_pointer_down: None,
            back_menu: ContextMenu::new(),
            back_entries: Vec::new(),
            menu_direction: None,
            history_request: 0,
            pending_history: None,
            document_revision: 0,
            show_settings: false,
            show_library: false,
            library_section: LibrarySection::History,
            library_filter: ui::LocalState::new(String::new()),
            structure_dirty: false,
            last_query: None,
            omni_index: omnibox::HistoryIndex::default(),
            omni_stale: true,
            suggest_debounce: omnibox::Debounce::default(),
            suggest_fetcher: omnibox::SuggestFetcher::default(),
            remote: Vec::new(),
            last_typed: String::new(),
            inline: None,
            select_completion: None,
            force_suggestions: false,
            content_rect: std::sync::Arc::new(std::sync::Mutex::new(None)),
            resource_navigation_invocation: None,
            resource_navigation_request: None,
            next_navigation_request: 0,
            explicit_navigation_requested: false,
            resource_navigation_result: None,
            resource_navigation_deadline: None,
            resource_pip_invocation: None,
            resource_pip_request: None,
            resource_pip_result: None,
            resource_pip_deadline: None,
            page_operation: None,
            page_operation_result: None,
            pending_replies: Vec::new(),
            page_request_seq: 0,
            pending_pip: None,
            pending_pip_region_update: None,
            published_pip_region: None,
            content_fullscreen: false,
            window_fullscreen_active: false,
            active_pip_element: None,
            pip_page_fallback: false,
            active_pip: false,
            pip_id: PIP_ID.into(),
            pip_request_seq: 0,
            active_pip_probe: None,
            active_pip_refresh_probe: None,
            next_pip_refresh_at: None,
            pip_feedback: None,
            pending_capture_studio: None,
            cursor: Point::ZERO,
            review_stage: None,
            compact_chrome: false,
            compact_breakpoint: ui::tokens::Metrics::BASE.compact_breakpoint,
            text_scale: 1.0,
            pending_commands: ui::LocalState::new(Vec::new()),
            motion_reduced: false,
            pending_controls: std::collections::VecDeque::new(),
            engine_problem: None,
            engine_setup: None,
            engine_setup_tried: false,
            engine_setup_shown: None,
            engine_installer: ui::webview::chromium::install::host_installer,
        }
    }

    /// Submit bookmark and preference edits through `dispatcher`.
    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Browser>,
    ) {
        self.dispatch = Some(dispatcher);
        self.structure_dirty = true;
    }

    /// Submit one bookmark or preference edit through the resource's door.
    fn submit(&self, submit: impl ui::framework::Submit<Browser>) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(submit);
        }
    }

    /// Present the document's bookmarks and preferences: the chrome shows
    /// them, and the engine follows the preferences.
    pub(crate) fn present_state(&mut self, state: &Browser) {
        let bookmarks = Bookmarks::of(&state.bookmarks);
        if bookmarks != self.bookmarks {
            self.bookmarks = bookmarks;
            self.structure_dirty = true;
        }
        self.apply_preferences(state.preferences.clone());
        self.site_exceptions = state.exceptions();
        self.site
            .present_exceptions(&self.engine, &self.site_exceptions, self.bootstrap_done);
    }

    /// Add the current page, unless it is already bookmarked. Removal stays
    /// on the library row; the toolbar action is the add.
    fn page_bookmark_action(&self) -> Option<Invoke<Browser>> {
        if self.current_uri.trim().is_empty() || self.bookmarks.contains(&self.current_uri) {
            return None;
        }
        Some(Browser::ADD_BOOKMARK.with(browser::AddBookmark {
            url: self.current_uri.clone(),
            title: Some(self.title.clone()),
        }))
    }

    /// `button` submits `action` through the resource's door; without one
    /// (nothing to edit, or no dispatcher) it does nothing.
    fn document_button(
        &self,
        button: ui::widget::Button,
        action: Option<Invoke<Browser>>,
    ) -> ui::widget::Button {
        match (action, self.dispatch.clone()) {
            (Some(action), Some(dispatch)) => {
                button.on_effect(move || dispatch.submit(action.clone()))
            }
            _ => button,
        }
    }

    /// "Bookmark this page", bound to the page this window shows: offered
    /// while it shows one that is not bookmarked yet.
    pub(crate) fn page_declarations(
        &self,
    ) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::Browser>> {
        if self.person_bookmarks.is_some() || self.page_bookmark_action().is_none() {
            return Vec::new();
        }
        let params = browser::AddBookmark::paths();
        Browser::ADD_BOOKMARK
            .bind(params.url, self.current_uri.clone())
            .and(params.title, Some(self.title.clone()))
            .declaration("bookmark-page", "Bookmark this page")
            .into_iter()
            .collect()
    }

    /// Document edits bound to the row `row`: the page's row turns tracker
    /// protection on or off for the site it shows.
    pub(crate) fn row_declarations(
        &self,
        row: &str,
    ) -> Vec<ui::tilo_resource::ActionDecl<Browser>> {
        if row != "browser.page" {
            return Vec::new();
        }
        let Some(origin) = crate::site_controls::origin(&self.current_uri) else {
            return Vec::new();
        };
        let protected = !self.site_exceptions.contains(&origin);
        Browser::site_switch(&origin, protected)
            .into_iter()
            .collect()
    }

    /// Stage the window for a screenshot scenario: the chrome state each one
    /// shows, as the engine, the network and the person would leave it.
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("pip-active") => self.debug_show_active_pip(),
            Some("pip-empty") => self.debug_show_empty_pip_feedback(),
            Some("settings") => self.debug_show_settings(),
            Some("history-paused") => self.debug_show_history_paused(),
            Some("loading") => self.debug_show_loading(),
            Some("error") => self.debug_show_load_error(),
            Some("permission") => self.debug_request_permission(),
            Some("suggestions") => {
                self.debug_force_suggestions(&crate::history::sample_entries());
                self.suggestions.move_selection(1);
            }
            Some("library-history") => self.debug_show_library("history"),
            Some("library-bookmarks") => self.debug_show_library("bookmarks"),
            Some("library-extensions") => self.debug_show_library("extensions"),
            Some("library-filtered") => {
                self.debug_show_library("history");
                self.debug_set_library_filter("rust");
            }
            Some(downloads) if downloads.starts_with("downloads-") => {
                self.debug_show_downloads(downloads.trim_start_matches("downloads-"));
            }
            _ => {}
        }
        self.structure_dirty = true;
    }

    /// Test hook: pin a suggestion list into view (bypasses focus/query)
    /// so the overlay can be rendered headlessly for inspection.
    pub fn debug_force_suggestions(&mut self, items: &[crate::history::HistoryEntry]) {
        self.suggestions
            .set_suggestions(items.iter().map(omnibox::Row::history).collect());
        self.force_suggestions = true;
    }

    /// Headless visual-review hook: show the settled no-video guidance note.
    pub fn debug_show_empty_pip_feedback(&mut self) {
        self.set_pip_feedback(Some(PipFeedback::persistent(PipFeedbackKind::NoVideo)));
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
        self.compact_chrome = is_compact_chrome(f64::from(width), self.compact_breakpoint);
        // The desktop Library cap also needs the viewport before its first
        // retained layout pass. Production windows already have this rect by
        // the time a user opens Library; headless review does not.
        self.rect = Rect::new(0.0, 0.0, f64::from(width), 0.0);
    }

    /// Populate and open a browser-library section for deterministic review.
    /// Stage the same bounded local filter that the framework and actions use.
    pub fn debug_set_library_filter(&mut self, query: &str) {
        self.library_filter.set(bounded_library_filter(query));
        self.structure_dirty = true;
    }

    pub fn debug_show_library(&mut self, section: &str) {
        // Visits over the past days, so the history shows its day groups.
        let now = u64::try_from(ui::clock::local_now().timestamp()).unwrap_or_default();
        for (url, title, ago) in [
            (
                "https://en.wikipedia.org/wiki/Web_browser",
                "Web browser - Wikipedia",
                4 * 24 * 3600,
            ),
            ("https://www.rust-lang.org/learn", "Learn Rust", 26 * 3600),
            (
                "https://developer.mozilla.org/en-US/docs/Web",
                "MDN Web Docs",
                3 * 3600,
            ),
            (
                "https://example.org/long-form",
                "Long-form reading",
                20 * 60,
            ),
        ] {
            self.history.record_at(url, title, now.saturating_sub(ago));
        }
        self.library_section = match section {
            "bookmarks" => LibrarySection::Bookmarks,
            "downloads" => LibrarySection::Downloads,
            "extensions" => LibrarySection::Extensions,
            _ => LibrarySection::History,
        };
        self.show_library = true;
        self.show_settings = false;
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

    /// Headless visual-review hook: the passwords panel, or the address
    /// autofill panel.
    pub fn debug_show_autofill(&mut self, addresses: bool) {
        self.review_stage = Some(ReviewStage::Autofill { addresses });
    }

    /// Headless visual-review hook: a site asking for the camera.
    pub fn debug_request_permission(&mut self) {
        self.capabilities.apply(WebViewEvent::PermissionRequested {
            id: 1,
            origin: "https://meet.example.org".into(),
            permissions: "camera and microphone".into(),
        });
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
        self.history_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
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

    /// Perform a browser operation and report inputs that the Browser is not
    /// permitted to hand to the web engine.  In particular, a browser window
    /// is not a file picker: its package declares only the web schemes, and
    /// passing `file:` through would bypass the portal boundary.
    fn set_loading_progress(&mut self, progress: f64) {
        let progress = progress.clamp(0.0, 1.0);
        if (self.progress < 1.0) != (progress < 1.0) {
            self.structure_dirty = true;
            self.loading_started = (progress < 1.0).then(ui::time::now);
        }
        self.progress = progress;
    }

    /// Whether the engine has not reported any progress for this load yet.
    fn loading_indeterminate(&self) -> bool {
        self.progress <= 0.0
    }

    /// The loading line under the toolbar, while a page shown in the
    /// window loads.
    fn loading_face(&self) -> Option<ui::widget::ProgressBarFace> {
        if self.progress >= 1.0
            || self.show_settings
            || self.show_library
            || self.load_error.is_some()
            || self.engine_problem.is_some()
        {
            return None;
        }
        Some(if self.loading_indeterminate() {
            ui::widget::ProgressBarFace::indeterminate(
                self.loading_started.unwrap_or_else(ui::time::now),
            )
        } else {
            ui::widget::ProgressBarFace::new(self.progress as f32)
        })
    }

    fn perform_browser_operation(&mut self, operation: BrowserOperation) -> Result<(), String> {
        self.perform_browser_operation_owned(operation, None)
    }

    fn cancel_owned_navigation(&mut self) {
        if let Some(request_id) = self.resource_navigation_request.take() {
            self.engine
                .send(WebViewCommand::CancelScopedNavigation { request_id });
        }
    }

    fn send_navigation(
        &mut self,
        command: NavigationCommand,
        owner: Option<u32>,
    ) -> Result<(), String> {
        if let Some(request) = owner {
            self.engine.navigate_owned(request, command)?;
        } else {
            self.engine.send(match command {
                NavigationCommand::Navigate { url } => WebViewCommand::Navigate(url),
                NavigationCommand::Back => WebViewCommand::GoBack,
                NavigationCommand::Forward => WebViewCommand::GoForward,
                NavigationCommand::Reload => WebViewCommand::Reload,
                NavigationCommand::HistoryEntry {
                    history_request_id,
                    id,
                } => WebViewCommand::NavigateToHistoryEntry {
                    request_id: history_request_id,
                    id,
                },
            });
        }
        self.explicit_navigation_requested = true;
        self.service_destination = None;
        self.service_redirect = None;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "browser operations share navigation admission and state transitions"
    )]
    fn perform_browser_operation_owned(
        &mut self,
        operation: BrowserOperation,
        owner: Option<u32>,
    ) -> Result<(), String> {
        if matches!(
            &operation,
            BrowserOperation::Back
                | BrowserOperation::Forward
                | BrowserOperation::Reload
                | BrowserOperation::Navigate { .. }
        ) {
            self.loading_stopped = false;
            self.structure_dirty |= self.daily.navigation_started(&self.engine);
            self.reader_view = false;
        }
        match operation {
            BrowserOperation::OpenServiceSession | BrowserOperation::ForgetServiceSession => {
                return Err("Open this account through its resource view action".into());
            }
            BrowserOperation::ReadServiceSession | BrowserOperation::ListServiceSessions => {
                return Err("Account status must use its resource action".into());
            }
            BrowserOperation::ReadContinuity | BrowserOperation::RetryContinuity => {
                return Err("Browser migration must use its resource action".into());
            }
            BrowserOperation::Back => {
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.send_navigation(NavigationCommand::Back, owner)?;
            }
            BrowserOperation::Forward => {
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.send_navigation(NavigationCommand::Forward, owner)?;
            }
            BrowserOperation::Reload => {
                let command = if self.load_error.is_some()
                    && !self.address_error_active
                    && url::Url::parse(&self.current_uri).is_ok_and(|url| {
                        matches!(url.scheme(), "http" | "https") && url.host().is_some()
                    }) {
                    NavigationCommand::Navigate {
                        url: self.navigation_target(&self.current_uri.clone())?,
                    }
                } else {
                    NavigationCommand::Reload
                };
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.send_navigation(command, owner)?;
            }
            BrowserOperation::Stop => {
                self.loading_stopped = true;
                self.engine.send(WebViewCommand::StopLoading);
                self.set_loading_progress(1.0);
                if let Some(invocation) = self.resource_navigation_invocation.take() {
                    self.resource_navigation_deadline = None;
                    self.cancel_owned_navigation();
                    self.resource_navigation_result =
                        Some(ui::tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: ui::tilo_actions::ActionFailure {
                                code: "browser.navigation_stopped".into(),
                                message: "Page loading stopped.".into(),
                                details: None,
                                retryable: true,
                            },
                        });
                }
            }
            BrowserOperation::OpenInNewTab { url, background } => {
                let target = self.navigation_target(url.trim())?;
                self.open_beside(target, background);
            }
            BrowserOperation::Navigate { url } => {
                let target = self.navigation_target(url.trim())?;
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.show_library = false;
                self.show_settings = false;
                self.structure_dirty = true;
                self.set_loading_progress(0.0);
                self.send_navigation(NavigationCommand::Navigate { url: target }, owner)?;
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
            operation @ (BrowserOperation::BookmarkPersonalPage { .. }
            | BrowserOperation::ImportPersonalBookmarks { .. }
            | BrowserOperation::RemovePersonalBookmark { .. }
            | BrowserOperation::RemovePersonalImportedNode { .. }
            | BrowserOperation::ShowPersonalBookmarkFolder { .. }) => {
                self.perform_personal_bookmark_operation(operation)?;
            }
            BrowserOperation::ClearHistory => {
                self.clear_history_durably()?;
            }
            BrowserOperation::RequestWebsiteDataClear => {
                self.engine.send(WebViewCommand::ClearWebsiteData);
            }
            BrowserOperation::TogglePictureInPicture => self.begin_pip_probe(),
            BrowserOperation::ToggleReaderView => {
                return Err("reader view reports its result; start it as an invocation".into());
            }
            BrowserOperation::ToggleHistoryPause => {
                self.history_paused
                    .fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
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
            BrowserOperation::Page(_) => {
                return Err("page actions report their result; start them as an invocation".into());
            }
            BrowserOperation::InstallEngine => {
                if self.engine_problem != Some(EngineProblem::NotInstalled) {
                    return Err("the browser engine is already installed".into());
                }
                // Under way: nothing to start. Failed: try again.
                if self.engine_setup.as_ref().is_none_or(|setup| {
                    matches!(setup.state(), crate::engine_setup::Setup::Failed(_))
                }) && !self.begin_engine_setup()
                {
                    // No host to ask (outside Tilo): the Store is the way.
                    ui::portal::open_uri_detached(ENGINE_INSTALL_URI);
                }
            }
        }
        Ok(())
    }

    /// Ask the host for the engine and follow it. Whether there was a host
    /// to ask.
    fn begin_engine_setup(&mut self) -> bool {
        self.engine_setup = (self.engine_installer)().map(|installer| {
            crate::engine_setup::EngineSetup::start(
                installer,
                Some(self.initial_url.clone()).filter(|url| is_supported_web_url(url)),
                ui::WakeHandle::new(),
            )
        });
        self.engine_setup_shown = None;
        self.structure_dirty = true;
        self.engine_setup.is_some()
    }

    /// First use without the engine installs it: asked for once when the
    /// window opens, followed each frame so the page area shows progress.
    fn drive_engine_setup(&mut self) {
        if self.engine_problem != Some(EngineProblem::NotInstalled) {
            return;
        }
        if !self.engine_setup_tried {
            self.engine_setup_tried = true;
            self.begin_engine_setup();
        }
        let now = self
            .engine_setup
            .as_ref()
            .map(crate::engine_setup::EngineSetup::state);
        if now != self.engine_setup_shown {
            self.engine_setup_shown = now;
            self.structure_dirty = true;
        }
    }

    fn page_loading(&self) -> bool {
        self.progress < 1.0 && !self.loading_stopped
    }

    /// Read and operate the page's content without its pixels: an outline
    /// whose interactive elements carry refs, and actions on those refs.
    fn start_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        request: PageRequest,
    ) -> Result<(), String> {
        if !request.valid() {
            return Err(format!(
                "`{}` names no element of the page outline, or arguments the page cannot take",
                request.action()
            ));
        }
        if self.page_operation.is_some() || self.page_operation_result.is_some() {
            return Err("another page action is already running in this tab".into());
        }
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        let now = std::time::Instant::now();
        self.page_operation = Some(PageOperation {
            invocation,
            request_id: self.page_request_seq,
            phase: PagePhase::Queued {
                request,
                until: now + PAGE_LOAD_WAIT,
            },
            deadline: now + PAGE_OPERATION_TIMEOUT,
            settled_while_sent: None,
        });
        self.drive_page_operation();
        Ok(())
    }

    /// Send a request once its page has settled, and bound every phase.
    fn drive_page_operation(&mut self) {
        let now = std::time::Instant::now();
        let loading = self.page_loading();
        let Some(operation) = self.page_operation.as_mut() else {
            return;
        };
        if let PagePhase::Queued { until, .. } = &operation.phase
            && (!loading || now >= *until)
            && let PagePhase::Queued { request, .. } =
                std::mem::replace(&mut operation.phase, PagePhase::Sent)
        {
            self.engine.send(WebViewCommand::Page {
                request_id: operation.request_id,
                request,
            });
            return;
        }
        if now < operation.deadline {
            return;
        }
        let Some(operation) = self.page_operation.take() else {
            return;
        };
        self.reader_request = None;
        self.page_operation_result = Some(match operation.phase {
            PagePhase::Navigating { reply } => self.page_success(
                operation.invocation,
                &reply,
                Some("the page it opened is still loading"),
            ),
            PagePhase::Queued { .. } | PagePhase::Sent => {
                ui::tilo_actions::InvocationUpdate::Failed {
                    invocation: operation.invocation,
                    error: ui::tilo_actions::ActionFailure {
                        code: "browser.page.timeout".into(),
                        message: "The page did not answer in time.".into(),
                        details: Some(serde_json::json!({
                            "timeout_seconds": PAGE_OPERATION_TIMEOUT.as_secs(),
                        })),
                        retryable: true,
                    },
                }
            }
        });
    }

    fn accept_page_result(&mut self, request_id: u32, result: Result<PageReply, PageFailure>) {
        let Some(operation) = self.page_operation.take_if(|operation| {
            operation.request_id == request_id && matches!(operation.phase, PagePhase::Sent)
        }) else {
            return;
        };
        if let Some(enabled) = self.reader_request.take()
            && result.is_ok()
        {
            self.reader_view = enabled;
            self.structure_dirty = true;
        }
        if let (Ok(reply), Some(settled)) = (&result, &operation.settled_while_sent)
            && reply.loading
        {
            let note = match settled {
                SettledLoad::Loaded => None,
                SettledLoad::Failed(error) => {
                    Some(format!("the page it opened failed to load: {error}"))
                }
            };
            self.page_operation_result =
                Some(self.page_success(operation.invocation, reply, note.as_deref()));
            return;
        }
        self.page_operation_result = Some(match result {
            Ok(reply) if reply.loading => {
                self.page_operation = Some(PageOperation {
                    phase: PagePhase::Navigating { reply },
                    deadline: std::time::Instant::now() + PAGE_NAVIGATION_WAIT,
                    ..operation
                });
                return;
            }
            Ok(reply) => self.page_success(operation.invocation, &reply, None),
            Err(failure) => ui::tilo_actions::InvocationUpdate::Failed {
                invocation: operation.invocation,
                error: ui::tilo_actions::ActionFailure {
                    retryable: matches!(failure.code.as_str(), "busy" | "timeout"),
                    code: format!("browser.page.{}", failure.code),
                    message: failure.message,
                    details: None,
                },
            },
        });
    }

    /// A page action that started a navigation completes once it settles, so
    /// the caller's next read sees the page the action opened.
    fn page_navigation_settled(&mut self, error: Option<&str>) {
        if let Some(operation) = self.page_operation.as_mut()
            && matches!(operation.phase, PagePhase::Sent)
        {
            operation.settled_while_sent = Some(match error {
                None => SettledLoad::Loaded,
                Some(error) => SettledLoad::Failed(error.to_owned()),
            });
            return;
        }
        let Some(operation) = self
            .page_operation
            .take_if(|operation| matches!(operation.phase, PagePhase::Navigating { .. }))
        else {
            return;
        };
        let PagePhase::Navigating { reply } = operation.phase else {
            return;
        };
        let note = error.map(|error| format!("the page it opened failed to load: {error}"));
        self.page_operation_result =
            Some(self.page_success(operation.invocation, &reply, note.as_deref()));
    }

    fn page_success(
        &self,
        invocation: ui::tilo_actions::InvocationId,
        reply: &PageReply,
        note: Option<&str>,
    ) -> ui::tilo_actions::InvocationUpdate {
        // After a navigation the tab's committed address is newer than the
        // engine's answer, which was taken as the navigation began.
        let url = if reply.loading && !self.current_uri.is_empty() {
            self.current_uri.clone()
        } else {
            reply.url.clone()
        };
        let message = note.map_or_else(
            || reply.message.clone(),
            |note| format!("{}; {note}", reply.message),
        );
        let mut value = serde_json::Map::new();
        value.insert("url".into(), url.into());
        value.insert("title".into(), self.title.as_str().into());
        value.insert("message".into(), message.into());
        value.insert("navigated".into(), reply.navigated.into());
        if let Some(outline) = &reply.outline {
            value.insert("outline".into(), outline.as_str().into());
        }
        if let Some(data) = &reply.data {
            value.insert("data".into(), data.clone());
        }
        // Everything here came from the page: data to reason about, never
        // instructions to follow.
        value.insert("provenance".into(), "web-content".into());
        ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: ui::tilo_actions::ActionOutput {
                value: Some(value.into()),
                resources: Vec::new(),
            },
        }
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

    /// Clear the history only when the person's record took the clear, so
    /// actions never report history as cleared when it will reappear.
    fn clear_history_durably(&mut self) -> Result<(), String> {
        if let Some(record) = &self.person_history
            && let Err(error) =
                record.invoke(&crate::history::BrowsingHistory::CLEAR_HISTORY.with(()))
        {
            let message =
                "Couldn’t clear history because Browser could not save the change. Previous history was kept."
                    .to_string();
            tracing::warn!("browser history clear was not persisted: {error}");
            self.history_clear_error = Some(message.clone());
            self.structure_dirty = true;
            return Err(message);
        }
        self.history.clear();
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
        walk(&[self.published()], wanted)
    }

    fn unit_binding(&self, resource: &str, action: &str) -> Option<ActionBinding<()>> {
        let resource = self.resource_by_id(resource)?;
        ActionBinding::unit_for(&resource, action).ok()
    }

    fn settings_action_bindings(&self) -> SettingsActionBindings {
        SettingsActionBindings {
            edits: self.dispatch.clone(),
            clear_history: self.unit_binding("browser.history", "clear-history"),
            clear_site_data: self.unit_binding("browser.settings", "request-site-data-clear"),
            close: self.unit_binding(crate::resource::Browser::KIND, "show-page"),
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

        let candidate =
            if !input.chars().any(char::is_whitespace) && looks_like_bare_web_host(input) {
                format!("https://{input}")
            } else {
                search_url(&self.settings.search_engine, input)
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

    /// What the anchored panel reported since the last sync: a dismissal, a
    /// choice in the no-video note, or the note running out.
    fn apply_panel_events(&mut self) {
        if let Some(kind) = self.panel_dismissed.get() {
            self.panel_dismissed.set(None);
            match kind {
                PanelKind::Credentials => self.deactivate_credentials(),
                PanelKind::Site => self.site.close(),
                // Dismissing a question is declining it.
                PanelKind::Prompt => self.capabilities.decline_prompt(&self.engine),
                PanelKind::Pip => self.pip_feedback = None,
                PanelKind::Extension => self.extensions.dismiss(),
            }
            self.structure_dirty = true;
        }
        if let Some(choice) = self.pip_choice.get() {
            self.pip_choice.set(None);
            match choice {
                PipChoice::FloatPage => self.float_current_page(),
                PipChoice::SelectArea => {
                    self.pending_capture_studio = Some(ui::widget::CaptureMode::Pip);
                    self.pip_feedback = None;
                }
            }
            self.structure_dirty = true;
        }
        // The note lingers while the pointer is over it.
        let hovered = self
            .anchored_panel
            .as_ref()
            .is_some_and(|panel| panel.get_rect().contains(self.cursor));
        if !hovered
            && self
                .pip_feedback
                .as_ref()
                .is_some_and(PipFeedback::is_expired)
        {
            self.pip_feedback = None;
            self.structure_dirty = true;
        }
    }

    fn set_pip_feedback(&mut self, feedback: Option<PipFeedback>) {
        self.pip_feedback = feedback;
        self.structure_dirty = true;
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
            self.set_pip_feedback(None);
            self.structure_dirty = true;
            return;
        }
        self.pip_request_seq = self.pip_request_seq.saturating_add(1);
        self.active_pip_probe = Some(self.pip_request_seq);
        self.set_pip_feedback(Some(PipFeedback::transient(PipFeedbackKind::Finding)));
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
            self.next_pip_refresh_at = Some(ui::time::now() + PIP_ELEMENT_REFRESH);
            return;
        }
        if self.active_pip_probe != Some(request_id) {
            return;
        }
        self.active_pip_probe = None;
        self.set_pip_feedback(None);

        let region = self.media_element_region(&element);
        if region.width() < 1.0 || region.height() < 1.0 {
            self.set_pip_feedback(Some(PipFeedback::transient(PipFeedbackKind::NoVideo)));
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
            id: self.pip_id.clone(),
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
        self.next_pip_refresh_at = Some(ui::time::now() + PIP_ELEMENT_REFRESH);
        self.structure_dirty = true;
        self.finish_pip_operation_success(request_id, "Picture in Picture opened.");
    }

    fn poll_active_pip_element(&mut self) {
        if !self.active_pip || self.pip_page_fallback || self.active_pip_refresh_probe.is_some() {
            return;
        }
        let due = self
            .next_pip_refresh_at
            .is_some_and(|deadline| ui::time::now() >= deadline);
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
        self.next_pip_refresh_at = Some(ui::time::now() + PIP_ELEMENT_REFRESH);
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
        self.set_pip_feedback(Some(PipFeedback::transient(PipFeedbackKind::NoVideo)));
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
            id: self.pip_id.clone(),
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
        self.set_pip_feedback(None);
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
        self.set_pip_feedback(Some(PipFeedback::transient(PipFeedbackKind::NoVideo)));
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

    /// Ordinary layout passes retain control state, including pending native paste replies.
    /// Rebuild when the view, theme or chrome structure changes.
    fn retain_composition(&mut self, theme: &Theme) -> Box<dyn Widget> {
        let key = CompositionKey {
            theme: theme.clone(),
            view: if self.show_settings {
                CompositionView::Settings
            } else if self.show_library {
                CompositionView::Library(self.library_section)
            } else {
                CompositionView::Page {
                    find: self.daily.open,
                }
            },
            hosted: ui::window_toolbar::is_hosted(),
            compact: self.compact_chrome,
            immersive: self.content_fullscreen && self.window_fullscreen_active,
            window_scale: self.window_scale,
        };
        if !self.composition_dirty
            && !self.structure_dirty
            && self.composition_key.as_ref() == Some(&key)
            && let Some(composed) = self.composed.take()
        {
            return composed;
        }
        let address = self.composed.element().and_then(address_state);
        let mut composed = self.build_composition(theme);
        if let Some((id, state)) = address {
            restore_address(composed.as_mut(), &id, &state);
        }
        self.composition_key = Some(key);
        self.composition_dirty = false;
        self.structure_dirty = false;
        composed
    }

    #[allow(
        clippy::too_many_lines,
        reason = "The browser chrome is intentionally composed in one ordered layout function."
    )]
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.content_fullscreen && self.window_fullscreen_active {
            return Box::new(
                WebView::new(self.engine.clone())
                    .with_scale_factor(self.window_scale)
                    .with_test_id("browser.surface"),
            );
        }
        // The web view must reach the window edges: no border, no gap. Only
        // the toolbar row keeps its inset (its own padding); the outer column
        // drops all padding and the inter-child gap so the page sits flush
        // under the toolbar and fills the rest of the window.
        let mut column = Column::new()
            .with_no_padding()
            .with_gap(theme.tokens.spacing.s0);
        // In a Tilo session the toolbar is the window bar's slots
        // (`window_bar`): the window draws no row of its own.
        if !ui::window_toolbar::is_hosted() {
            column = column.push(self.build_toolbar(theme));
        }
        // Messages from the window: over a web page they run edge to edge
        // under the toolbar, over the settings or library page they are the
        // first part of the page.
        let on_page = self.show_settings || self.show_library;
        let mut notices: Vec<crate::notice::Notice> = Vec::new();
        if self.service_pending.is_some() {
            notices.push(crate::service_sessions::PendingView::notice());
        }
        if let Some(notice) = self
            .service_view
            .as_ref()
            .and_then(crate::service_sessions::ServiceView::notice)
        {
            notices.push(notice);
        }
        // A private window is marked by its tinted toolbar and the Private
        // chip in it (`build_toolbar`, `draw`), not by a banner that would
        // take a row from every page.
        if let Some(message) = &self.private_window_notice {
            notices.push(
                crate::notice::Notice::warning("Couldn’t open a private window")
                    .with_message(message.clone()),
            );
        }
        if self.close_blocked {
            notices.push(
                crate::notice::Notice::warning("This tab has an active download").with_message(
                    "Finish or cancel it in Downloads in the library, then close the tab.",
                ),
            );
        }
        // The history notice would only repeat the Library's own control.
        if self.history_paused() && !self.show_library {
            notices.push(
                crate::notice::Notice::neutral("History paused")
                    .with_message("Pages in this window are not saved to browsing history."),
            );
        }
        if !on_page {
            for notice in &notices {
                column = column.push(notice.widget(true));
            }
        }
        if self.daily.open && !on_page {
            column = column.push(self.daily.bar(theme));
        }
        // The content region is either the settings pane or the live page.
        let content: Box<dyn Widget> = if self.show_settings {
            Box::new(
                SettingsPane::with_settings(&self.settings)
                    .with_presentation(self.settings_presentation.clone())
                    .with_autofill_request(self.credentials.open_request())
                    .with_action_bindings(self.settings_action_bindings())
                    .with_history_clear_error(self.history_clear_error.clone())
                    .with_notices(notices),
            )
        } else if self.show_library {
            self.build_library_composition(theme, &notices)
        } else if let Some(problem) = self.engine_problem {
            engine_problem_state(
                problem,
                self.unit_binding("browser.page", "install-engine"),
                self.engine_setup_shown.as_ref(),
            )
        } else if let Some(error) = &self.load_error {
            let recovery = EmptyState::error("Couldn’t open this page", error)
                .with_icon(Icon::Wifi)
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
                    .with_gap(theme.tokens.spacing.s0)
                    .with_fill_remaining_height()
                    .push(recovery),
            )
        } else {
            // Assistive technology reads the document by its title, as in
            // any browser; an untitled page keeps the generic name.
            let page_name = if self.title.trim().is_empty() {
                "Web content".to_owned()
            } else {
                self.title.clone()
            };
            Box::new(
                WebView::new(self.engine.clone())
                    .with_scale_factor(self.window_scale)
                    .with_accessible_name(page_name)
                    .with_test_id("browser.surface"),
            )
        };
        // Loading is drawn on the toolbar's bottom edge (`draw`), not as a
        // band of its own: a band pushed the page down when a load began
        // and pulled it back up when it ended, so every link moved the
        // whole page twice.
        column = column.push(content);
        Box::new(column)
    }

    /// Put a staged review panel on screen, once page loads can no longer
    /// close it.
    fn apply_review_stage(&mut self) {
        let Some(stage) = self.review_stage.take() else {
            return;
        };
        let url = if self.current_uri.is_empty() {
            self.address.with(Clone::clone)
        } else {
            self.current_uri.clone()
        };
        match stage {
            ReviewStage::Autofill { addresses } => {
                // The panels open from rows of the settings page, so that is
                // the page behind them.
                self.show_settings = true;
                self.show_library = false;
                self.credentials.stage_for_review(addresses, &url);
            }
        }
        self.structure_dirty = true;
    }

    /// Follow the document's preferences: the engine takes what changed,
    /// and the settings pane shows them.
    fn apply_preferences(&mut self, next: Settings) {
        if next != self.settings {
            // Before its first sync the window bootstraps the engine with
            // whatever it holds then.
            if self.bootstrap_done && next.adblock_enabled != self.settings.adblock_enabled {
                self.set_adblock(next.adblock_enabled);
            }
            if self.bootstrap_done && next.clear_on_exit != self.settings.clear_on_exit {
                self.engine
                    .send(WebViewCommand::SetClearOnExit(next.clear_on_exit));
            }
            // A window that has not loaded its first page yet opens on the
            // homepage the person chose, unless its launch named a page.
            if !self.bootstrap_done
                && self.initial_url == self.settings.homepage
                && is_supported_web_url(&next.homepage)
            {
                self.initial_url.clone_from(&next.homepage);
                self.address.set(next.homepage.clone());
            }
            // A launch that asked for a web search searches on the engine
            // the person chose, once their preferences are in.
            if !self.bootstrap_done
                && let Some(query) = &self.initial_search
            {
                self.initial_url = search_url(&next.search_engine, query);
                self.address.set(self.initial_url.clone());
            }
            self.settings = next;
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
    /// user may be editing; focus alone does not imply an unsent draft).
    fn url_input_focused(&self) -> bool {
        self.control_focused(URL_INPUT_ID)
    }

    /// Whether the address field's go button was pressed since the last
    /// look; clears it.
    fn take_address_submit(&mut self) -> bool {
        self.composed
            .element_mut()
            .and_then(|c| ui::widget::find_test_id_mut(c, URL_INPUT_ID))
            .and_then(Widget::as_any_mut)
            .and_then(|widget| widget.downcast_mut::<ui::widget::TextInput>())
            .is_some_and(ui::widget::TextInput::take_submit)
    }

    fn control_focused(&self, id: &str) -> bool {
        self.composed
            .element()
            .and_then(|c| ui::widget::find_test_id(c, id))
            .is_some_and(Widget::is_focused)
    }

    /// Fold engine events into this window's chrome state. Engine state is a
    /// device-local external boundary, never a collaborative document write.
    #[allow(
        clippy::too_many_lines,
        reason = "Engine events are folded in delivery order."
    )]
    fn apply_engine_events(&mut self) {
        if self
            .download_save_due
            .is_some_and(|due| ui::time::now() >= due)
        {
            self.persist_downloads_now();
            self.download_save_due = None;
        }
        self.poll_download_picker();
        if let Some(view) = self.service_view.as_mut()
            && view.poll_timeout()
        {
            self.structure_dirty = true;
        }
        let editing = self.url_input_focused();
        for event in self
            .engine
            .drain_events()
            .into_iter()
            .chain(self.engine.drain_download_events())
        {
            let event = match event {
                WebViewEvent::LoadFinishedAt { uri } => {
                    if uri != self.current_uri {
                        continue;
                    }
                    WebViewEvent::LoadFinished
                }
                event => event,
            };
            match event {
                WebViewEvent::ServiceSessionResult(reply) => {
                    if let Some(view) = self.service_view.as_mut()
                        && view.apply(&reply)
                    {
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::DevToolsResult {
                    request_id,
                    ok,
                    body,
                } => {
                    self.structure_dirty |= self.extensions.on_result(request_id, ok, &body);
                }
                WebViewEvent::CredentialResult(reply) => {
                    let requests = self.credentials.apply(reply);
                    crate::credentials::Credentials::send(&self.engine, requests);
                    self.structure_dirty = true;
                }
                event @ (WebViewEvent::SiteStatus(_) | WebViewEvent::SiteResult { .. }) => {
                    let reload_started = matches!(
                        &event,
                        WebViewEvent::SiteResult { action, success: true, .. }
                            if action == "reload"
                    );
                    let accepted = self.site.apply(event);
                    self.structure_dirty |= accepted;
                    if accepted && reload_started {
                        self.loading_stopped = false;
                        self.clear_address_error();
                        self.set_loading_progress(0.0);
                        self.structure_dirty |= self.daily.navigation_started(&self.engine);
                        self.reader_view = false;
                    }
                }
                WebViewEvent::TabClosed | WebViewEvent::TabDetached => {
                    self.native_closed = true;
                }
                WebViewEvent::TabCloseBlocked => {
                    self.close_blocked = true;
                    self.structure_dirty = true;
                }
                WebViewEvent::ContextMenu {
                    id,
                    x,
                    y,
                    link,
                    items,
                } => {
                    if let Some(surface) = self.find_rect("browser.surface") {
                        self.page_menu
                            .open(id, x, y, link, items, surface, self.rect);
                    } else {
                        self.engine
                            .send(WebViewCommand::ContextMenuAction { id, command: 0 });
                    }
                }
                WebViewEvent::ContextMenuClosed { id } => self.page_menu.dismiss(id),
                event @ (WebViewEvent::FindResult { .. } | WebViewEvent::ZoomChanged(_)) => {
                    self.structure_dirty |= self.daily.apply(&event);
                }
                WebViewEvent::NavigationHistory {
                    request_id,
                    direction,
                    entries,
                } => {
                    if self.pending_history.is_some_and(|pending| {
                        pending.request_id == request_id
                            && pending.direction == direction
                            && pending.document_revision == self.document_revision
                    }) && match direction {
                        NavigationDirection::Back => self.can_go_back,
                        NavigationDirection::Forward => self.can_go_forward,
                    } {
                        self.pending_history = None;
                        self.open_navigation_menu(direction, entries);
                    }
                }
                event @ (WebViewEvent::PermissionRequested { .. }
                | WebViewEvent::PermissionClosed { .. }
                | WebViewEvent::NativeDownloadRequested { .. }
                | WebViewEvent::DownloadUpdated { .. }
                | WebViewEvent::DownloadRuntime { .. }) => {
                    self.capabilities.apply(event);
                    self.structure_dirty = true;
                }
                WebViewEvent::UriChanged(uri) => {
                    self.site.navigation(&uri);
                    let requests = self.credentials.navigation(&uri);
                    crate::credentials::Credentials::send(&self.engine, requests);
                    self.structure_dirty = true;
                    self.document_revision = self.document_revision.wrapping_add(1);
                    self.favicon = None;
                    self.favicon_png = None;
                    self.media_playing = false;
                    self.media_document_generation = None;
                    if uri != self.current_uri {
                        self.title.clear();
                        self.loading_stopped = false;
                        self.structure_dirty |= self.daily.navigation_started(&self.engine);
                        self.reader_view = false;
                    }
                    self.pending_history = None;
                    self.back_menu.close();
                    self.menu_direction = None;
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
                    let follows_navigation = self.address.with(|value| {
                        value == &self.current_uri
                            || self.address_navigation_value.as_ref() == Some(value)
                    });
                    self.address_navigation_value = None;
                    if !editing || follows_navigation {
                        self.address.set(uri.clone());
                    }
                    if uri.starts_with("chrome-extension://") {
                        self.show_library = false;
                        self.show_settings = false;
                        self.structure_dirty = true;
                    }
                    self.current_uri = uri;
                }
                WebViewEvent::TitleChanged(title)
                    if !self.forced_load_error && !self.forced_loading =>
                {
                    // The page surface is named after its title.
                    self.structure_dirty |= self.title != title;
                    self.title = title;
                }
                WebViewEvent::FaviconChanged {
                    page_url,
                    width,
                    height,
                    png,
                } if page_url == self.current_uri
                    || (self.current_uri.is_empty() && page_url == self.initial_url) =>
                {
                    if let Some(image) = decode_favicon_png(&png, width, height) {
                        self.favicon = Some(image);
                        self.favicon_png = Some(png.to_vec());
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::AudioMutedChanged(muted) => {
                    self.audio_muted = muted;
                    self.structure_dirty = true;
                }
                WebViewEvent::MediaPlayingChanged {
                    page_url,
                    document_generation,
                    playing,
                } => {
                    if page_url == self.current_uri
                        && self
                            .media_document_generation
                            .is_none_or(|current| document_generation >= current)
                    {
                        self.media_document_generation = Some(document_generation);
                        self.media_playing = playing;
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::FullscreenChanged(active) => {
                    if self.content_fullscreen != active {
                        self.content_fullscreen = active;
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::RendererTerminated => {
                    self.deactivate_credentials();
                    if self.content_fullscreen {
                        self.content_fullscreen = false;
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::LoadProgress(p)
                    if !self.forced_loading
                        && !self.loading_stopped
                        && self.load_error.is_none() =>
                {
                    self.set_loading_progress(p);
                }
                WebViewEvent::LoadFinishedAt { .. }
                | WebViewEvent::FaviconChanged { .. }
                | WebViewEvent::TitleChanged(_)
                | WebViewEvent::LoadProgress(_)
                | WebViewEvent::PaintSnapshot { .. }
                | WebViewEvent::PaintResource { .. }
                | WebViewEvent::DocumentSize { .. } => {}
                WebViewEvent::LoadFinished => {
                    if self.load_error.is_some() {
                        continue;
                    }
                    if !self.forced_load_error && !self.address_error_active {
                        self.load_error = None;
                    }
                    if !self.forced_loading {
                        self.set_loading_progress(1.0);
                    }
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if self.browsing_mode.persists_personal_state()
                        && !self.history_paused()
                        && self.current_uri.starts_with("http")
                        && let Some(record) = &self.person_history
                    {
                        let visit = crate::history::BrowsingHistory::VISIT.with(
                            crate::history::browsing_history::Visit {
                                url: self.current_uri.clone(),
                                title: self.title.clone(),
                                at: tilo_clock::unix_now(),
                            },
                        );
                        if let Err(error) = record.invoke(&visit) {
                            tracing::warn!(%error, "browser visit was not recorded");
                        }
                        self.follow_history();
                        self.follow_personal_bookmarks();
                    }
                    self.page_navigation_settled(None);
                }
                WebViewEvent::LoadFailed { uri, error } => {
                    self.site.navigation(&uri);
                    self.favicon = None;
                    self.favicon_png = None;
                    self.media_playing = false;
                    self.media_document_generation = None;
                    let follows_navigation = self.address.with(|value| {
                        value == &self.current_uri
                            || self.address_navigation_value.as_ref() == Some(value)
                    });
                    if !editing || follows_navigation {
                        self.address.set(uri.clone());
                    }
                    self.current_uri.clone_from(&uri);
                    self.address_navigation_value = None;
                    self.set_loading_progress(1.0);
                    tracing::warn!("load failed for {uri}: {error}");
                    self.title = format!("Failed to load {uri}");
                    self.load_error = Some(error.chars().take(240).collect());
                    self.structure_dirty = true;
                    self.page_navigation_settled(Some(&error));
                }
                WebViewEvent::NavigationOutcome {
                    request_id,
                    uri,
                    error,
                } => {
                    if self.resource_navigation_request == Some(request_id)
                        && let Some(invocation) = self.resource_navigation_invocation.take()
                    {
                        self.resource_navigation_request = None;
                        self.resource_navigation_deadline = None;
                        self.resource_navigation_result = Some(match error {
                            Some(message) => ui::tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: ui::tilo_actions::ActionFailure {
                                    code: "browser.navigation_failed".into(),
                                    message,
                                    details: Some(serde_json::json!({ "uri": uri })),
                                    retryable: true,
                                },
                            },
                            None => ui::tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: ui::tilo_actions::ActionOutput {
                                    value: Some(serde_json::Value::String(uri)),
                                    resources: Vec::new(),
                                },
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
                WebViewEvent::PageResult { request_id, result } => {
                    self.accept_page_result(request_id, result);
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
                        Some(ui::time::now() + std::time::Duration::from_secs(1));
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
        if let Err(error) = self.downloads.save() {
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
    fn install_blocking_scripts(&self) {
        let scripts = std::sync::Arc::new(crate::blocker::user_scripts());
        self.engine
            .send(if self.engine.supports_native_filtering() {
                WebViewCommand::SetBlockingScripts(scripts)
            } else {
                WebViewCommand::SetUserScripts(scripts)
            });
    }
    fn set_adblock(&self, enabled: bool) {
        if enabled {
            crate::blocker::spawn_filter_loader(self.engine.clone());
            self.install_blocking_scripts();
        } else {
            if self.engine.supports_native_filtering() {
                self.engine.send(WebViewCommand::SetNativeFilterRules {
                    rules: std::sync::Arc::new(String::new()),
                    enabled: Some(false),
                });
            } else {
                self.engine.send(WebViewCommand::SetContentFilter(None));
            }
            self.engine
                .send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
                    Vec::new(),
                )));
        }
    }

    /// Forget everything the omnibox derived from the text being typed.
    fn reset_omnibox(&mut self) {
        self.suggestions.clear();
        self.suggest_debounce.cancel();
        self.remote.clear();
        self.last_query = None;
        self.last_typed.clear();
        self.inline = None;
        self.select_completion = None;
    }

    /// Refresh the address-bar suggestions while the address input is being
    /// edited; hide them otherwise. Runs every frame, does bounded work, and
    /// never waits on the network: the engine's suggestions arrive later
    /// through `suggest_fetcher`.
    fn update_suggestions(&mut self) {
        if self.force_suggestions {
            return; // headless overlay render — keep the pinned list
        }
        let text = if ui::window_toolbar::is_hosted() {
            // The address is edited in the window's bar: what the bar said
            // is being typed there.
            self.bar_query.clone().unwrap_or_default()
        } else if self.show_settings || !self.url_input_focused() {
            String::new()
        } else {
            self.address.with(Clone::clone)
        };
        // The bar echoing the completion we put there is not new typing.
        let typed = match &self.inline {
            Some((typed, full)) if *full == text => typed.clone(),
            _ => {
                self.inline = None;
                text
            }
        };
        let query = typed.trim();
        // Only suggest while the user is editing a *query* — not when the
        // bar merely shows the loaded page's URL, and not for an empty bar.
        let editing = !query.is_empty() && query != self.current_uri.trim();
        if !editing {
            if self.last_query.is_some() || self.suggestions.is_visible() {
                self.reset_omnibox();
            }
            return;
        }
        let query = query.to_owned();
        let mut dirty = self.last_query.as_deref() != Some(query.as_str());
        if dirty {
            self.on_query_changed(&query);
        }
        if let Some(answer) = self.suggest_fetcher.take()
            && self.suggest_debounce.is_current(answer.generation)
            && answer.query == query
        {
            self.remote = answer.suggestions;
            dirty = true;
        }
        if let Some((generation, wanted)) = self
            .suggest_debounce
            .due(ui::time::now(), omnibox::DEBOUNCE)
            && let Some(endpoint) = omnibox::suggest_endpoint(&self.settings.search_engine, &wanted)
        {
            self.suggest_fetcher.fetch(generation, wanted, endpoint);
        }
        if dirty || self.omni_stale {
            self.rebuild_rows(&query);
        }
    }

    /// The typed text changed: restart the quiet period for the engine's
    /// suggestions and keep only the old ones that still fit.
    fn on_query_changed(&mut self, query: &str) {
        let lower = query.to_lowercase();
        self.remote
            .retain(|suggestion| suggestion.to_lowercase().starts_with(&lower));
        if query.chars().count() >= 2 && !omnibox::looks_like_address(query) {
            self.suggest_debounce.typed(query, ui::time::now());
        } else {
            self.suggest_debounce.cancel();
            self.remote.clear();
        }
    }

    /// Select the untyped tail of an inline completion, once the address
    /// field holds the completed text.
    fn select_inline_completion(&mut self) {
        fn select(widget: &mut dyn Widget, state: &str) -> bool {
            if widget.test_id() == Some(URL_INPUT_ID) {
                ui::undo::set_state(widget, state);
                return true;
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| select(child, state))
        }
        let Some((end, start)) = self.select_completion.take() else {
            return;
        };
        if let Some(c) = self.composed.element_mut() {
            select(c, &format!("{end}:b:{start}"));
        }
    }

    /// Match, merge and complete for `query`; set the dropdown's rows.
    fn rebuild_rows(&mut self, query: &str) {
        if self.omni_stale {
            self.omni_index.rebuild(self.history.entries());
            self.omni_stale = false;
        }
        let now = tilo_clock::unix_now();
        let matches = self.omni_index.search(query, omnibox::MAX_ROWS, now);
        let mut rows = omnibox::build_rows(
            query,
            &matches,
            &self.remote,
            &self.current_uri,
            omnibox::MAX_ROWS,
        );
        // Complete inline only when the person is adding characters; after a
        // deletion the text is theirs.
        let growing = query.len() > self.last_typed.len() && query.starts_with(&self.last_typed);
        if growing
            && self.inline.is_none()
            && let Some(full) = omnibox::inline_completion(query, matches.first())
        {
            self.select_completion = Some((full.len(), query.len()));
            self.address.set(full.clone());
            self.inline = Some((query.to_owned(), full));
        }
        if let Some((typed, full)) = &self.inline
            && typed == query
        {
            omnibox::lead_with_address(&mut rows, full);
        }
        query.clone_into(&mut self.last_typed);
        self.last_query = Some(query.to_owned());
        self.suggestions.set_suggestions(rows);
    }

    /// Open a chosen dropdown row.
    fn choose_row(&mut self, row: omnibox::Row) {
        self.suggestions.clear();
        self.last_query = None;
        self.inline = None;
        let destination = match row.kind {
            omnibox::RowKind::Search | omnibox::RowKind::Suggestion => {
                search_url(&self.settings.search_engine, &row.url)
            }
            omnibox::RowKind::Go | omnibox::RowKind::History => row.url,
        };
        self.address.set(destination.clone());
        self.queue_navigate(destination);
    }

    /// Rect of the address `TextInput` in the composed tree, for anchoring
    /// the suggestion overlay under it.
    fn address_input_rect(&self) -> Option<Rect> {
        self.find_rect(URL_INPUT_ID)
    }

    /// The anchored panel's content and the controls it opens beside, nearest
    /// first, or `None` when nothing is open. A popover is placed against the
    /// control that summoned it; when that control is not on screen (a
    /// compact window moves some into the overflow menu), the next stands in.
    fn anchored_panel_parts(
        &self,
        width: f32,
        max_height: f32,
    ) -> Option<(PanelKind, Box<dyn Widget>, Vec<String>)> {
        let width = width.max(1.0);
        if self.credentials.open {
            let opener = if self.credentials.addresses_open() {
                "browser.address.open"
            } else {
                "browser.credentials.open"
            };
            return Some((
                PanelKind::Credentials,
                self.credentials.panel(width, max_height),
                anchor_ids(&[opener, toolbar::OVERFLOW_ID, URL_INPUT_ID]),
            ));
        }
        if self.site.open {
            return Some((
                PanelKind::Site,
                self.site.panel(width, max_height),
                anchor_ids(&["browser.site.info", toolbar::OVERFLOW_ID, URL_INPUT_ID]),
            ));
        }
        if let Some((content, mut anchors)) = self.extensions.panel(width) {
            anchors.push(toolbar::OVERFLOW_ID.to_owned());
            anchors.push(URL_INPUT_ID.to_owned());
            return Some((PanelKind::Extension, content, anchors));
        }
        if let Some(prompt) = self.capabilities.prompt(&self.engine) {
            let anchors = if self.capabilities.prompt_is_permission() {
                anchor_ids(&["browser.site.info", URL_INPUT_ID, toolbar::OVERFLOW_ID])
            } else {
                anchor_ids(&[
                    "browser.download-status",
                    toolbar::OVERFLOW_ID,
                    URL_INPUT_ID,
                ])
            };
            return Some((PanelKind::Prompt, prompt, anchors));
        }
        let feedback = self.pip_feedback.as_ref()?;
        let note = |text: &str| -> Box<dyn Widget> {
            Box::new(ui::widget::Text::new(text).with_role(ui::tokens::TextRole::Body))
        };
        let content: Box<dyn Widget> = match feedback.kind {
            PipFeedbackKind::Finding => note("Finding the active video…"),
            PipFeedbackKind::NoVideo => {
                let choose = |id: &str, label: &str, icon: Icon, choice: PipChoice| {
                    let pending = self.pip_choice.clone();
                    Button::new(id, label)
                        .with_icon(icon)
                        .on_effect(move || pending.set(Some(choice)))
                };
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_rhythm(ui::tokens::Rhythm::Sibling)
                        .push(note("No visible video on this page"))
                        .push(Box::new(
                            ui::widget::Actions::new(choose(
                                "browser.pip.float-page",
                                "Float page",
                                Icon::PictureInPicture2,
                                PipChoice::FloatPage,
                            ))
                            .secondary(choose(
                                "browser.pip.select-area",
                                "Select area",
                                Icon::MousePointer2,
                                PipChoice::SelectArea,
                            )),
                        )),
                )
            }
        };
        Some((
            PanelKind::Pip,
            content,
            anchor_ids(&[toolbar::OVERFLOW_ID, URL_INPUT_ID]),
        ))
    }

    /// The popover for whatever panel is open, anchored to its control.
    fn build_anchored_panel(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let window = self.rect;
        // The popover pads its content on every side.
        let pad = 2.0 * spacing.s4;
        let room = window.width() as f32 - 2.0 * spacing.s3 - pad;
        let width = metrics.panel_width_list.min(room);
        let toolbar_bottom = self
            .find_rect("browser.toolbar")
            .map_or(0.0, |toolbar| toolbar.y1);
        let max_height =
            (window.height() as f32 - toolbar_bottom as f32 - spacing.s3 - pad).max(160.0);
        let (kind, content, anchors) = self.anchored_panel_parts(width, max_height)?;
        // A control of the toolbar row opens its panel below the whole row,
        // so the row stays uncovered.
        let anchor = anchors.iter().find_map(|id| self.find_rect(id));
        let anchor = anchor.map_or_else(
            || Rect::new(window.x0, toolbar_bottom, window.x1, toolbar_bottom),
            |control| {
                if control.y1 < toolbar_bottom {
                    Rect::new(control.x0, control.y0, control.x1, toolbar_bottom)
                } else {
                    control
                }
            },
        );
        let dismissed = self.panel_dismissed.clone();
        let mut popover = Popover::with_content(
            Container::new()
                .with_no_padding()
                .with_width(width)
                .with_child(content),
        )
        .anchored(anchor)
        .open(true)
        .on_dismiss(move || dismissed.set(Some(kind)));
        // Before the first layout the window has no size to stay inside.
        if window.area() > 0.0 {
            popover = popover.with_viewport(window.inset(-f64::from(spacing.s3)));
        }
        Some(Box::new(popover))
    }

    fn request_navigation_history(&mut self, direction: NavigationDirection) {
        self.history_request = (self.history_request % i32::MAX as u32) + 1;
        self.pending_history = Some(PendingHistory {
            request_id: self.history_request,
            direction,
            document_revision: self.document_revision,
        });
        self.back_menu.close();
        self.menu_direction = None;
        self.suggestions.clear();
        self.engine.send(WebViewCommand::RequestNavigationHistory {
            request_id: self.history_request,
            direction,
        });
    }

    fn open_navigation_menu(
        &mut self,
        direction: NavigationDirection,
        entries: Vec<ui::webview::NavigationEntry>,
    ) {
        let control = match direction {
            NavigationDirection::Back => "browser.back",
            NavigationDirection::Forward => "browser.forward",
        };
        let Some(anchor) = self.find_rect(control) else {
            return;
        };
        let rows = ((self.rect.y1 - anchor.y1 - 20.0) / 32.0)
            .floor()
            .clamp(0.0, 12.0) as usize;
        self.back_entries = entries.into_iter().take(rows).collect();
        self.back_menu = ContextMenu::new();
        for entry in &self.back_entries {
            let label = if entry.title.trim().is_empty() {
                &entry.url
            } else {
                &entry.title
            };
            // ContextMenu paints one line; cap graphemes to keep labels inside its fixed width.
            let label: String = label.chars().take(28).collect();
            self.back_menu.push(MenuItem::action(label, || {}));
        }
        if !self.back_entries.is_empty() {
            self.menu_direction = Some(direction);
            self.back_menu.open_anchored(anchor, self.rect);
        }
    }

    fn choose_navigation_entry(&mut self, index: usize) {
        if let Some(entry) = self.back_entries.get(index) {
            let id = entry.id;
            self.address_navigation_value = Some(self.address.with(Clone::clone));
            self.clear_address_error();
            self.set_loading_progress(0.0);
            self.engine.send(WebViewCommand::NavigateToHistoryEntry {
                request_id: self.history_request,
                id,
            });
        }
        self.back_menu.close();
        self.menu_direction = None;
    }

    fn tick_back_hold(&mut self, now: ui::time::Instant) {
        if let Some(direction) = self.navigation_hold.tick(now) {
            self.request_navigation_history(direction);
        }
    }

    fn sync_search_and_suggestions(&mut self) {
        self.search_index.sync_all(&self.history, &self.downloads);
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
        let platform =
            ui::platform::PlatformConventions::default().command_modifier_down(modifiers);
        if platform && !modifiers.alt_key() {
            use crate::daily_controls::Action;
            if matches!(&key.logical_key, WKey::Character(c) if c == "[" || c == "]") {
                self.queue_unit_operation(
                    "browser.page",
                    if matches!(&key.logical_key, WKey::Character(c) if c == "[") {
                        "back"
                    } else {
                        "forward"
                    },
                );
                return true;
            }
            let action = match &key.logical_key {
                WKey::Character(c) if c.eq_ignore_ascii_case("f") => Some(Action::Open),
                WKey::Character(c) if c.eq_ignore_ascii_case("g") => {
                    Some(Action::Next(!modifiers.shift_key()))
                }
                WKey::Character(c) if c == "+" || c == "=" => Some(Action::Zoom(1)),
                WKey::Character(c) if c == "-" => Some(Action::Zoom(-1)),
                WKey::Character(c) if c == "0" => Some(Action::Zoom(0)),
                WKey::Character(c) if c.eq_ignore_ascii_case("d") => {
                    if self.person_bookmarks.is_some() {
                        self.queue_unit_operation("browser.page", "bookmark-personal-page");
                        return true;
                    }
                    if let Some(action) = self.page_bookmark_action() {
                        self.submit(action);
                    }
                    return true;
                }
                _ => None,
            };
            if let Some(action) = action {
                self.daily.act(action, &self.engine);
                self.structure_dirty = true;
                return true;
            }
        }
        if key.logical_key == WKey::Named(WNamedKey::F3) {
            self.daily.act(
                crate::daily_controls::Action::Next(!modifiers.shift_key()),
                &self.engine,
            );
            return true;
        }
        if self.daily.open
            && (key.logical_key == WKey::Named(WNamedKey::Escape)
                || (key.logical_key == WKey::Named(WNamedKey::Enter)
                    && self.control_focused(crate::daily_controls::FIND_INPUT)))
        {
            self.daily.act(
                if key.logical_key == WKey::Named(WNamedKey::Escape) {
                    crate::daily_controls::Action::Close
                } else {
                    crate::daily_controls::Action::Next(!modifiers.shift_key())
                },
                &self.engine,
            );
            self.structure_dirty = true;
            return true;
        }
        if key.logical_key == WKey::Named(WNamedKey::Escape) && self.progress < 1.0 {
            self.queue_unit_operation("browser.page", "stop");
            return true;
        }
        // Reader view, on Firefox's key for it.
        if platform
            && modifiers.alt_key()
            && matches!(&key.logical_key, WKey::Character(c) if c.eq_ignore_ascii_case("r"))
        {
            self.queue_unit_operation("browser.page", "reader-view");
            return true;
        }
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
        self.focus_control(URL_INPUT_ID)
    }

    fn focus_control(&mut self, id: &str) -> bool {
        fn clear(widget: &mut dyn Widget) {
            widget.set_focus(false);
            for child in widget.children_mut() {
                clear(child);
            }
        }
        fn focus(widget: &mut dyn Widget, id: &str) -> bool {
            let Some(widget) = ui::widget::find_test_id_mut(widget, id) else {
                return false;
            };
            widget.set_focus(true);
            if let Some((_, text)) = widget.text_review() {
                if text.is_empty() {
                    ui::undo::set_state(widget, "0");
                } else {
                    ui::undo::set_state(widget, &format!("{}:b:0", text.len()));
                }
            }
            true
        }
        self.composed.element_mut().is_some_and(|composed| {
            clear(composed);
            focus(composed, id)
        })
    }

    fn focus_control_preserving_selection(&mut self, id: &str) {
        fn restore(widget: &mut dyn Widget, id: &str, value: &str) {
            if widget.test_id() == Some(id) {
                ui::undo::set_state(widget, value);
            }
            for child in widget.children_mut() {
                restore(child, id, value);
            }
        }
        let saved = self
            .composed
            .element()
            .and_then(|widget| ui::widget::find_test_id(widget, id))
            .and_then(ui::undo::get_state);
        self.focus_control(id);
        if let (Some(value), Some(widget)) = (saved, self.composed.element_mut()) {
            restore(widget, id, &value);
        }
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
        // F6 moves to the address bar on its own, as in every browser.
        let f6 = key.state == ElementState::Pressed
            && key.logical_key == WKey::Named(WNamedKey::F6)
            && !modifiers.alt_key();
        if !(requested || f6) {
            return false;
        }
        if ui::window_toolbar::is_hosted() {
            // The address is the window bar's location: it edits there.
            ui::window_toolbar::edit_location();
            return true;
        }
        !self.show_settings && !self.show_library && self.focus_address_input()
    }

    /// Rect of the composed-tree widget with `test_id == id`, if present.
    fn find_rect(&self, id: &str) -> Option<Rect> {
        self.composed
            .element()
            .and_then(|c| ui::widget::find_test_id(c, id))
            .or_else(|| {
                self.anchored_panel
                    .as_ref()
                    .and_then(|panel| ui::widget::find_test_id(panel.as_ref(), id))
            })
            .map(Widget::get_rect)
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
        *self.content_rect.lock_recover() = frac;
    }
}

/// Why live browsing has no running engine, in the terms the page area
/// shows a person. What a developer needs goes to the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineProblem {
    /// The Chromium engine runtime is not installed.
    NotInstalled,
    /// The engine is installed but did not start.
    Failed,
}

/// Exceptional repair of the offline engine baseline. Store resolves the
/// installed Browser root and reviews its exact declared optional component;
/// opening this handoff never consents to a payload download.
pub(crate) const ENGINE_INSTALL_URI: &str =
    "tilo-store:runtime/so.tilo.browser/tilo-runtime/2026.10.17-chromium";

/// Live browsing runs the Chromium engine runtime
/// ([`ui::webview::chromium::runtime`]); there is no other engine and no
/// simulated page. `TILO_BROWSER_PAINT=1` draws its pages from the paint
/// stream (the engine's layout as a display list, drawn by this toolkit)
/// instead of its pixels: what a web client of this host sees. Either way the
/// session can share the page as that stream.
fn default_engine(mode: BrowsingMode) -> (SharedEngine, Option<EngineProblem>) {
    #[cfg(test)]
    if let Some(engine) = test_window_engine::take() {
        return (engine, None);
    }
    let (engine, problem) = native_engine(mode);
    if problem.is_some() {
        return (engine, problem);
    }
    if std::env::var("TILO_BROWSER_PAINT").as_deref() == Ok("1") {
        return (ui::webview::paint::PaintEngine::new(engine), None);
    }
    // Drawn from pixels here; captured as a display list only while the
    // session shares the window with someone watching (a web member).
    (ui::webview::paint::PaintEngine::streaming(engine), None)
}

/// The engine a test's next live window drives instead of the native one,
/// so a test can run Browser in its real app host without starting a
/// browser process.
#[cfg(test)]
pub(crate) mod test_window_engine {
    use std::cell::RefCell;

    use ui::webview::SharedEngine;

    thread_local! {
        static NEXT: RefCell<Option<SharedEngine>> = const { RefCell::new(None) };
    }

    /// The next live window on this thread drives `engine`.
    pub(crate) fn install(engine: SharedEngine) {
        NEXT.with_borrow_mut(|next| *next = Some(engine));
    }

    pub(super) fn take() -> Option<SharedEngine> {
        NEXT.with_borrow_mut(Option::take)
    }
}

/// Deterministic rendering is confined to `new_headless`.
fn native_engine(mode: BrowsingMode) -> (SharedEngine, Option<EngineProblem>) {
    if let Err(missing) = ui::webview::chromium::runtime::locate() {
        tracing::warn!(hint = %missing.developer_hint(), "browser engine is not installed");
        return (
            unavailable_engine(mode, EngineProblem::NotInstalled),
            Some(EngineProblem::NotInstalled),
        );
    }
    let started = if mode.is_private() {
        crate::storage::explicit_root()
            .ok_or_else(|| "Private browser root was not configured".to_string())
            .and_then(ui::webview::chromium::ChromiumEngine::new_private)
            .map(|engine| engine as SharedEngine)
    } else if let Some(root) = crate::storage::explicit_root() {
        ui::webview::chromium::ChromiumEngine::with_profile_extensions(
            &root.join("chromium"),
            &crate::extensions::approved_paths(),
        )
        .map(|engine| engine as SharedEngine)
    } else {
        ui::webview::chromium::ChromiumEngine::new_with_extensions(
            &crate::extensions::approved_paths(),
        )
        .map(|engine| engine as SharedEngine)
    };
    engine_or_error(started)
}

fn engine_or_error(result: Result<SharedEngine, String>) -> (SharedEngine, Option<EngineProblem>) {
    match result {
        Ok(engine) => (engine, None),
        Err(error) => {
            tracing::error!(%error, "browser engine failed to start");
            (
                ui::webview::chromium::UnavailableEngine::new(user_message(EngineProblem::Failed)),
                Some(EngineProblem::Failed),
            )
        }
    }
}

/// An engine that renders nothing and answers navigation with the problem.
/// A private window's disposable root is released at once: nothing will
/// ever write to it.
fn unavailable_engine(mode: BrowsingMode, problem: EngineProblem) -> SharedEngine {
    if mode.is_private()
        && let Some(root) = crate::storage::explicit_root()
    {
        ui::webview::chromium::release_private_root(root);
    }
    ui::webview::chromium::UnavailableEngine::new(user_message(problem))
}

fn user_message(problem: EngineProblem) -> String {
    match problem {
        EngineProblem::NotInstalled => ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE,
        EngineProblem::Failed => ui::webview::chromium::runtime::FAILED_MESSAGE,
    }
    .to_owned()
}

/// The page area while the engine is missing or stopped: what happened,
/// and the one thing a person can do about it.
fn engine_problem_state(
    problem: EngineProblem,
    install: Option<ActionBinding<()>>,
    setup: Option<&crate::engine_setup::Setup>,
) -> Box<dyn Widget> {
    if let (EngineProblem::NotInstalled, Some(setup)) = (problem, setup) {
        return engine_setup_state(setup, install);
    }
    let state = match problem {
        EngineProblem::NotInstalled => {
            let state = EmptyState::new(ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE)
                .with_icon(Icon::Download)
                .with_message(ui::i18n!("browser-engine-repair-message"))
                .with_action(ui::i18n!("browser-engine-repair-action"))
                .with_action_id("browser.install-engine");
            match install {
                Some(binding) => state.on_action(binding),
                None => state,
            }
        }
        EngineProblem::Failed => EmptyState::new(ui::webview::chromium::runtime::FAILED_MESSAGE)
            .with_icon(Icon::Info)
            .with_message(ui::i18n!("browser-engine-reopen")),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .with_fill_remaining_height()
            .push(Box::new(state)),
    )
}

/// The page area while the host fetches the engine: what is happening and
/// how far along, in the toolkit's own empty state and progress bar.
fn engine_setup_state(
    setup: &crate::engine_setup::Setup,
    retry: Option<ActionBinding<()>>,
) -> Box<dyn Widget> {
    use crate::engine_setup::Setup;
    use ui::webview::chromium::install::InstallPhase;
    let mut column = Column::new()
        .with_no_padding()
        .with_gap(0.0)
        .with_fill_remaining_height();
    match setup {
        Setup::Fetching {
            phase,
            received,
            total,
        } => {
            let message = match (phase, total) {
                (InstallPhase::Preparing, _) => "Preparing…".to_owned(),
                (InstallPhase::Installing, _) => "Installing…".to_owned(),
                (InstallPhase::Downloading, Some(total)) => format!(
                    "Downloading, {} of {}",
                    megabytes(*received),
                    megabytes(*total)
                ),
                (InstallPhase::Downloading, None) => {
                    format!("Downloading, {}", megabytes(*received))
                }
            };
            column = column.push(Box::new(
                EmptyState::new("Getting the browser engine")
                    .with_icon(Icon::Download)
                    .with_message(message)
                    .busy(),
            ));
            let bar = setup.fraction().map_or_else(
                ui::widget::ProgressBar::indeterminate,
                ui::widget::ProgressBar::new,
            );
            column = column.push(Box::new(bar.with_label("Browser engine")));
        }
        Setup::Reopened => {
            column = column.push(Box::new(
                EmptyState::new("The browser engine is ready")
                    .with_icon(Icon::Info)
                    .with_message("Browser opened in a new window. You can close this one."),
            ));
        }
        Setup::Failed(error) => {
            let state = EmptyState::error("Couldn’t get the browser engine", error)
                .with_icon(Icon::Download)
                .with_action("Try again")
                .with_action_id("browser.install-engine");
            column = column.push(Box::new(match retry {
                Some(binding) => state.on_action(binding),
                None => state,
            }));
        }
    }
    Box::new(column)
}

fn megabytes(bytes: u64) -> String {
    ui::format::size(bytes)
}

fn address_state(widget: &dyn Widget) -> Option<(String, String)> {
    if matches!(
        widget.test_id(),
        Some(URL_INPUT_ID | crate::daily_controls::FIND_INPUT)
    ) && widget.is_focused()
    {
        return ui::undo::get_state(widget)
            .map(|state| (widget.test_id().unwrap_or(URL_INPUT_ID).to_owned(), state));
    }
    widget
        .children()
        .iter()
        .find_map(|child| address_state(*child))
}
fn restore_address(widget: &mut dyn Widget, id: &str, state: &str) {
    if widget.test_id() == Some(id) {
        widget.set_focus(true);
        ui::undo::set_state(widget, state);
        return;
    }
    for child in widget.children_mut() {
        restore_address(child, id, state);
    }
}

impl BrowserWidget {
    /// The page to show again when this window reopens or continues on
    /// another device. A private window leaves nothing behind.
    pub(crate) fn restorable_page(&self) -> Option<Vec<u8>> {
        if self.browsing_mode.is_private() {
            return None;
        }
        let url = self.continuation_url()?;
        serde_json::to_vec(&serde_json::json!({ "url": url })).ok()
    }

    /// Show the page [`Self::restorable_page`] saved. Only a web page
    /// without credentials in its address is restored.
    pub(crate) fn restore_page(&mut self, saved: &[u8]) -> bool {
        if self.browsing_mode.is_private() {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(saved) else {
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

impl Continuable for BrowserWidget {
    fn continuation_metadata(
        &self,
        _state: &AutoCommit,
        _root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
            bytes: self.restorable_page()?,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _state: &mut ui::widget::Edits<'_>,
        _root: &ObjId,
    ) -> bool {
        metadata.schema_version == ui::collaboration::semantic_schema::WEB_LOCATION_V1
            && self.restore_page(&metadata.bytes)
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
            .map(|region| vec![(self.pip_id.clone(), region)])
            .unwrap_or_default()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.active_pip
            .then(|| self.pip_id.clone())
            .into_iter()
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        match message {
            ui::floating_surface::FloatingSurfaceHostMessage::Adopted { id, .. }
                if id == &self.pip_id =>
            {
                self.active_pip = true;
            }
            ui::floating_surface::FloatingSurfaceHostMessage::Closed { id }
            | ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                if id == &self.pip_id =>
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
    fn has_pending_visual_work(&self) -> bool {
        self.service_catalog_pending.is_some()
            || {
                {
                    self.service_pending.is_some()
                        || self
                            .service_view
                            .as_ref()
                            .is_some_and(crate::service_sessions::ServiceView::is_preparing)
                }
            }
            || self
                .composed
                .element()
                .is_some_and(Widget::has_pending_visual_work)
    }

    ui::provides!(
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::controls::ActionControls
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.sync_frame(state, path);
        self.settle_replies();
    }
    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.pending_chrome_focus = None;
        let page_focused = self.control_focused("browser.surface");
        // The tiers and the labelled widths follow the text scale, which is
        // only known here; the one-frame capture seed set them at 1×.
        self.text_scale = theme.text_scale;
        self.compact_breakpoint = theme.tokens.metrics.compact_breakpoint;
        self.suggestions.set_theme(theme);
        if self.rect.width() > 0.0 {
            self.compact_chrome = is_compact_chrome(self.chrome_width(), self.compact_breakpoint);
        }
        // Anchor the popover to the controls currently on screen before laying
        // out either the retained composition or its replacement.
        let anchored_panel = self.build_anchored_panel(theme);
        let mut composed = self.retain_composition(theme);
        let child_node = composed.build_layout(taffy, theme, node_map);
        composed.set_motion_reduced(self.motion_reduced);
        self.composed.replace(composed);
        if let Some(id) = self.daily.focus_pending.take() {
            self.focus_control(id);
            if id == crate::daily_controls::FIND_INPUT
                && let Some(cursor) = self.daily.take_buffered_cursor()
                && let Some(composed) = self.composed.element_mut()
            {
                restore_address(composed, id, &cursor.to_string());
            }
            self.pending_chrome_focus = node_map.iter().find_map(|(node, widget)| {
                (unsafe { &**widget }.test_id() == Some(id)).then_some((*node, id))
            });
        } else if page_focused {
            self.focus_control("browser.surface");
            self.pending_chrome_focus = node_map.iter().find_map(|(node, widget)| {
                (unsafe { &**widget }.test_id() == Some("browser.surface"))
                    .then_some((*node, "browser.surface"))
            });
        }

        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child_node` was just built in this one above.
        let mut children = vec![child_node];
        self.anchored_panel = None;
        if let Some(mut panel) = anchored_panel {
            let panel_node = panel.build_layout(taffy, theme, node_map);
            // The popover places itself against its anchor; the node only
            // registers it with the tree.
            #[allow(
                clippy::unwrap_used,
                reason = "panel node was just created in this tree"
            )]
            let mut panel_style = taffy.style(panel_node).unwrap().clone();
            panel_style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(panel_node, panel_style);
            children.push(panel_node);
            self.anchored_panel = Some(panel);
        }
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let compact = is_compact_chrome(self.chrome_width(), self.compact_breakpoint);
        if compact != self.compact_chrome {
            self.compact_chrome = compact;
            self.structure_dirty = true;
        }
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = taffy.children(node)
            && let Some(&child_node) = children.first()
        {
            c.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
        if let Some(panel) = &mut self.anchored_panel
            && let Ok(children) = taffy.children(node)
            && let Some(&panel_node) = children.get(1)
        {
            panel.apply_layout(taffy, panel_node, Point::new(self.rect.x0, self.rect.y0));
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
        let coefficients = transform.as_coeffs();
        let scale = coefficients[0].hypot(coefficients[1]);
        if scale.is_finite() && scale > 0.0 {
            self.window_scale = scale as f32;
        }
        // A private window's toolbar is tinted, as Safari and Chrome mark
        // theirs: under the controls, so they keep their own contrast. The
        // directional veneer darkens a light ground and lightens a dark one.
        if self.browsing_mode.is_private()
            && let Some(rect) = self.find_rect("browser.toolbar")
        {
            ui::paint::fill_rect(scene, transform, rect, theme.tokens.colors.overlay_medium);
        }
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
        // One hairline under the toolbar row separates it from the page;
        // while the page loads, the loading line takes its place.
        if let Some(rect) = self.find_rect("browser.toolbar") {
            ui::widget::toolbar::paint_band(scene, transform, theme, rect);
            if let Some(face) = self.loading_face() {
                let height = f64::from(theme.tokens.metrics.chrome_rule_px.max(1.0)) * 2.0;
                face.paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(rect.x0, rect.y1 - height, rect.x1, rect.y1),
                );
            }
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
        if let Some(c) = self.composed.element_mut() {
            c.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // The popover paints its own material, shadow and content.
        if let Some(panel) = &mut self.anchored_panel {
            panel.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // Re-anchor under the address bar every draw (layout may not have
        // re-run since the suggestions changed).
        if let Some(r) = self.address_input_rect() {
            self.suggestions
                .set_anchor(Point::new(r.x0, r.y1 + 2.0), r.width() as f32);
        }
        self.suggestions
            .draw_overlay(scene, transform, text_renderer, theme, presence);

        self.page_menu
            .menu
            .draw(scene, transform, text_renderer, theme, presence);
        self.back_menu
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        // An indeterminate loading line sweeps; a determinate one moves
        // only when the engine reports progress.
        self.loading_face()
            .is_some_and(|_| self.loading_indeterminate() && !self.motion_reduced)
            || self.navigation_hold.waiting()
            || self.suggest_debounce.is_pending()
            || self.suggest_fetcher.in_flight()
            || self
                .composed
                .element()
                .is_some_and(Widget::needs_animation_frame)
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Toggling the settings pane swaps the content widget → rebuild.
        let own = std::mem::take(&mut self.structure_dirty);
        self.composition_dirty |= own;
        own || self.composed.take_structure_dirty()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if let Some(composed) = self.composed.element_mut() {
            composed.set_motion_reduced(reduced);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.window_scale = ctx.scale_factor as f32;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
        }
        if let Some((node, id)) = self.pending_chrome_focus.take() {
            self.focus_control_preserving_selection(id);
            *ctx.focus_target = Some(node);
        }
        if let Some(panel) = &mut self.anchored_panel {
            // The popover owns the pointer and the keyboard while it is up:
            // Escape and a click outside dismiss it, and nothing reaches the
            // page behind.
            if let Ok(children) = ctx.taffy.children(ctx.node_id)
                && let Some(&panel_node) = children.get(1)
            {
                panel.handle_event(event, &mut ctx.reborrow(ctx.path, panel_node));
            }
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
                    | WindowEvent::Ime(_)
            ) {
                *ctx.handled = true;
                return;
            }
        }
        if self.page_menu.menu.is_open() {
            self.page_menu.menu.handle_event(event, ctx);
            if let Some(index) = self.page_menu.menu.take_selection() {
                self.page_menu.choose(index, &self.engine);
            } else if !self.page_menu.menu.is_open() {
                self.page_menu.cancel(&self.engine);
            }
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            ) {
                *ctx.handled = true;
                return;
            }
        }
        let navigation_hit = [
            (NavigationDirection::Back, self.can_go_back, "browser.back"),
            (
                NavigationDirection::Forward,
                self.can_go_forward,
                "browser.forward",
            ),
        ]
        .into_iter()
        .find_map(|(direction, enabled, id)| {
            (enabled && self.find_rect(id).is_some_and(|r| r.contains(self.cursor)))
                .then_some(direction)
        });
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && (key.logical_key == WKey::Named(WNamedKey::ContextMenu)
                || (key.logical_key == WKey::Named(WNamedKey::F10)
                    && ctx.modifiers.state().shift_key()))
            && let Some(direction) = [
                (NavigationDirection::Back, self.can_go_back, "browser.back"),
                (
                    NavigationDirection::Forward,
                    self.can_go_forward,
                    "browser.forward",
                ),
            ]
            .into_iter()
            .find_map(|(direction, enabled, id)| {
                (enabled && self.control_focused(id)).then_some(direction)
            })
        {
            self.request_navigation_history(direction);
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => {
                self.navigation_hold.cancel();
                self.navigation_pointer_down = None;
                self.pending_history = None;
                self.back_menu.close();
                self.menu_direction = None;
            }
            WindowEvent::CursorMoved { .. }
                if self.navigation_pointer_down.is_some()
                    && navigation_hit != self.navigation_pointer_down =>
            {
                self.navigation_hold.cancel();
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == WKey::Named(WNamedKey::Escape) =>
            {
                self.navigation_hold.cancel();
                self.pending_history = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if navigation_hit.is_some() => {
                if let Some(direction) = navigation_hit {
                    self.request_navigation_history(direction);
                }
                *ctx.handled = true;
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if navigation_hit.is_some() && !self.back_menu.is_open() => {
                let Some(direction) = navigation_hit else {
                    return;
                };
                self.navigation_pointer_down = Some(direction);
                self.navigation_hold.press(ui::time::now(), direction);
                if self.motion_reduced {
                    // Reduced motion disables the host's animation pump; this
                    // one-shot wake still recognizes a stationary long press.
                    let wake = ui::WakeHandle::new();
                    let _ = std::thread::Builder::new()
                        .name("browser-navigation-hold".into())
                        .spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(450));
                            wake.wake();
                        });
                }
                *ctx.handled = true;
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.navigation_pointer_down.is_some() => {
                self.tick_back_hold(ui::time::now());
                self.navigation_pointer_down = None;
                if let Some(direction) = self.navigation_hold.release()
                    && navigation_hit == Some(direction)
                {
                    self.queue_unit_operation(
                        "browser.page",
                        match direction {
                            NavigationDirection::Back => "back",
                            NavigationDirection::Forward => "forward",
                        },
                    );
                }
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        if self.back_menu.is_open() {
            self.back_menu.handle_event(event, ctx);
            if let Some(index) = self.back_menu.take_selection() {
                self.choose_navigation_entry(index);
            }
            // Consume modal input so keys and pointer releases cannot reach the page.
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            ) {
                *ctx.handled = true;
                return;
            }
        }
        if self.handle_address_shortcut(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        // Conventional browser navigation keys. These must be handled before
        // forwarding into the page: an embedded site should not be able to
        // steal the browser's own history controls. The queued invocation is
        // the same canonical action the toolbar exposes to history, agents,
        // and collaboration.
        if self.handle_browser_shortcut(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        // Cmd/Ctrl+F can be followed by text before the host rebuilds the bar.
        // Retain those characters and place the caret after them on first layout.
        if self.daily.focus_pending == Some(crate::daily_controls::FIND_INPUT)
            && let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && !ctx.modifiers.state().super_key()
            && !ctx.modifiers.state().control_key()
        {
            if key.logical_key == WKey::Named(WNamedKey::Enter) {
                *ctx.handled = true;
                return;
            }
            if let WKey::Character(text) = &key.logical_key {
                self.daily.buffer_text(text);
                *ctx.handled = true;
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
                &mut ui::widget::EventCtx {
                    handled: &mut ov_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if let Some(row) = self.suggestions.take_chosen() {
                self.choose_row(row);
                *ctx.handled = true;
                return;
            }
            if ov_handled {
                *ctx.handled = true;
                return;
            }
        }

        // The first click into the address bar selects the whole address (a
        // second click places the caret), as in every browser.
        let click_into_address = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && !self.url_input_focused()
            && self
                .find_rect(URL_INPUT_ID)
                .is_some_and(|rect| rect.contains(self.cursor));

        if click_into_address {
            // The press only puts the whole address under the caret,
            // selected: handed to the field as well, its release would
            // collapse the selection where the press landed, and what the
            // person types would be appended to the old address.
            self.focus_address_input();
            self.swallow_address_release = true;
            *ctx.handled = true;
            return;
        }
        if self.swallow_address_release
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            self.swallow_address_release = false;
            *ctx.handled = true;
            return;
        }
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }

        // Enter in the focused address bar, or its go button, submits it
        // (no suggestion picked).
        let go_pressed = self.take_address_submit();
        if go_pressed
            || matches!(event, WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == WKey::Named(WNamedKey::Enter)
                    && self.url_input_focused())
        {
            self.navigate_to_input();
            self.reset_omnibox();
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.page_menu.menu.is_open() {
            return self.page_menu.accessibility();
        }
        if !self.back_menu.is_open() {
            return self.suggestions.accessibility().map(|node| {
                AccessNode::new(Role::Group)
                    .with_name("Browser")
                    .with_children(vec![node])
            });
        }
        let direction = self.menu_direction?;
        let rows = self
            .back_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let label = if entry.title.is_empty() {
                    &entry.url
                } else {
                    &entry.title
                };
                AccessNode::new(Role::MenuItem)
                    .with_name(label)
                    .with_action_target(format!(
                        "browser.navigation-history:{}:{}:{}",
                        self.history_request,
                        direction.as_str(),
                        entry.id
                    ))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.back_menu.hovered() == Some(index)),
                    )
            })
            .collect();
        let name = match direction {
            NavigationDirection::Back => "Back history",
            NavigationDirection::Forward => "Forward history",
        };
        Some(AccessNode::new(Role::Group).with_children(vec![
                AccessNode::new(Role::Menu)
                    .with_name(name)
                    .with_children(rows),
            ]))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if self
            .suggestions
            .perform_accessibility_action(target, action)
        {
            // This queues Browser's existing typed `browser.page/navigate`
            // action — AT-SPI does not get a parallel imperative navigation
            // route.
            if let Some(row) = self.suggestions.take_chosen() {
                self.choose_row(row);
            }
            return true;
        }
        if matches!(action, ui::a11y::Action::Activate)
            && self.page_menu.activate(target, &self.engine)
        {
            return true;
        }
        if !self.back_menu.is_open() || !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some((request, rest)) = target
            .strip_prefix("browser.navigation-history:")
            .and_then(|value| value.split_once(':'))
        else {
            return false;
        };
        let Some((direction, id)) = rest.split_once(':') else {
            return false;
        };
        if request.parse::<u32>().ok() != Some(self.history_request) {
            return false;
        }
        if NavigationDirection::from_wire(Some(direction)) != self.menu_direction {
            return false;
        }
        let Some(index) = self
            .back_entries
            .iter()
            .position(|entry| Some(entry.id) == id.parse::<u32>().ok())
        else {
            return false;
        };
        self.choose_navigation_entry(index);
        true
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let feedback_expiry = self
            .pip_feedback
            .as_ref()
            .filter(|feedback| !feedback.persistent)
            .map(|feedback| feedback.started + PIP_FEEDBACK_LIFETIME);
        // An operation an effect waits on times out even if nothing else
        // wakes the window.
        let waiting = (!self.pending_replies.is_empty())
            .then(|| {
                [
                    self.resource_navigation_deadline,
                    self.resource_pip_deadline,
                    self.page_operation
                        .as_ref()
                        .map(|operation| operation.deadline),
                ]
            })
            .into_iter()
            .flatten()
            .flatten();
        self.site
            .deadline()
            .into_iter()
            .chain(feedback_expiry)
            .chain(self.credentials.deadline())
            .chain(waiting)
            .chain(
                self.children()
                    .into_iter()
                    .filter_map(Widget::next_wake_deadline),
            )
            .min()
    }
    fn modal_capture(&self) -> Option<Rect> {
        if let Some(panel) = &self.anchored_panel {
            return Some(panel.get_rect());
        }
        self.page_menu
            .menu
            .modal_capture()
            .or_else(|| self.back_menu.modal_capture())
            .or_else(|| self.suggestions.modal_capture())
            .or_else(|| self.composed.element().and_then(Widget::modal_capture))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .element()
            .map(|c| c as &dyn Widget)
            .into_iter()
            .chain(
                self.anchored_panel
                    .as_ref()
                    .map(|p| p.as_ref() as &dyn Widget),
            )
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .element_mut()
            .map(|c| c as &mut (dyn Widget + '_))
            .into_iter()
            .chain(
                self.anchored_panel
                    .as_mut()
                    .map(|p| p.as_mut() as &mut (dyn Widget + '_)),
            )
            .collect()
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
}

impl BrowserWidget {
    /// The index of history pages and downloads, shared with the opener of
    /// their search hits.
    pub(crate) fn search_index(&self) -> crate::search_provider::BrowserSearchIndex {
        self.search_index.clone()
    }

    /// Every history page and download as a search entry.
    pub(crate) fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let mut entries = self.search_index.entries();
        entries.extend(crate::personal_bookmarks::search(&self.personal_bookmarks));
        entries
    }

    /// The row a history or download hit opens, beyond the published page
    /// of rows too.
    pub(crate) fn search_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.search_index
            .resource(id)
            .or_else(|| crate::personal_bookmarks::resolve(&self.personal_bookmarks, id))
            .map(ui::framework::LiveRow::from_resource)
    }

    /// What only this window knows, as rows beneath the browser resource:
    /// the page it shows, its history session, history, downloads,
    /// extensions and the library and settings presentations, each with the
    /// view and effect operations it offers. Bookmark rows add their
    /// "open" to the document's bookmarks.
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.published()
            .children
            .into_iter()
            .map(ui::framework::LiveRow::from_resource)
            .collect()
    }

    /// [`Self::live_rows`] beneath the resource root they join, which the
    /// window's own controls bind to.
    #[allow(
        clippy::too_many_lines,
        reason = "Resource definitions stay together so their action contracts remain discoverable."
    )]
    pub(crate) fn published(&self) -> Resource {
        use crate::effects::BrowserEffect as E;
        use ui::tilo_resource::Effect as _;
        const MATERIALIZED_LIMIT: usize = 100;
        let mut page = Resource::new(
            "browser.page",
            "browser.page",
            if self.title.is_empty() {
                "Web page"
            } else {
                &self.title
            },
        );
        if self.person_bookmarks.is_some()
            && !self.current_uri.trim().is_empty()
            && !self
                .personal_bookmarks
                .bookmarks
                .contains(&self.current_uri)
        {
            page.actions.push(
                E::BookmarkPersonalPage {
                    url: self.current_uri.clone(),
                    title: self.title.clone(),
                }
                .bound(),
            );
        }
        let back = E::Back.bound();
        page.actions.push(if self.can_go_back {
            back
        } else {
            back.unavailable("history_boundary", "No previous page")
        });
        let forward = E::Forward.bound();
        page.actions.push(if self.can_go_forward {
            forward
        } else {
            forward.unavailable("history_boundary", "No next page")
        });
        page.actions.push(E::Reload.bound());
        let stop = E::Stop.bound();
        page.actions.push(if self.progress < 1.0 {
            stop
        } else {
            stop.unavailable("history_boundary", "Page is idle")
        });
        page.actions
            .push(E::Navigate { url: String::new() }.bound_asking(&["url"]));
        page.actions.push(
            E::OpenTab {
                url: String::new(),
                background: None,
            }
            .bound_asking(&["url", "background"]),
        );
        if self.engine_problem == Some(EngineProblem::NotInstalled) {
            page.actions.push(E::InstallEngine.bound());
        }
        let mut pip = E::PictureInPicture.bound();
        if self.active_pip {
            pip.label = "Close Picture in Picture".into();
        }
        page.actions.push(pip);
        let web_page = is_supported_web_url(&self.current_uri);
        let mut reader = E::ReaderView.bound();
        if self.reader_view {
            reader.label = "Close reader view".into();
        }
        page.actions.push(if web_page {
            reader
        } else {
            reader.unavailable("no_web_page", "Open a web page first")
        });
        let element = String::new();
        for (spec, asking) in [
            (E::ReadPage, &[][..]),
            (
                E::ClickPage {
                    element: element.clone(),
                },
                &["element"][..],
            ),
            (
                E::FillPage {
                    element: element.clone(),
                    text: String::new(),
                },
                &["element", "text"][..],
            ),
            (
                E::SelectPage {
                    element: element.clone(),
                    values: Vec::new(),
                },
                &["element", "values"][..],
            ),
            (
                E::PressPage {
                    key: String::new(),
                    element: None,
                },
                &["key", "element"][..],
            ),
            (
                E::FillPageSecret {
                    element: element.clone(),
                    text: String::new(),
                },
                &["element", "text"][..],
            ),
            (
                E::SubmitPage {
                    element,
                    cross_origin: None,
                },
                &["element", "cross_origin"][..],
            ),
            (E::ReadPageChanges, &[][..]),
            (E::ReadPageElementAt { x: 0.0, y: 0.0 }, &["x", "y"][..]),
            (
                E::WaitPageSettled {
                    quiet_ms: None,
                    timeout_ms: None,
                },
                &["quiet_ms", "timeout_ms"][..],
            ),
            (E::ReadPageConsole { since: None }, &["since"][..]),
            (E::ReadPageNetwork { since: None }, &["since"][..]),
        ] {
            let spec = spec.bound_asking(asking);
            page.actions.push(if web_page {
                spec
            } else {
                spec.unavailable("no_web_page", "Open a web page first")
            });
        }

        let mut session = Resource::new(
            "browser.session",
            "browser.history-pause",
            if self.history_paused() {
                "History paused"
            } else {
                "History recording"
            },
        );
        let mut pause = E::TogglePause.bound();
        if self.history_paused() {
            pause.label = "Resume history in this window".into();
        }
        session.actions.push(pause);

        let mut history = Resource::new("browser.history", "browser.history", "History");
        history.actions.push(E::ShowHistory.bound());
        let clear = E::ClearHistory.bound();
        history.actions.push(if self.history.is_empty() {
            clear.unavailable("already_empty", "Browsing history is already empty")
        } else {
            clear
        });
        for entry in self.history.entries().iter().rev().take(MATERIALIZED_LIMIT) {
            let title = if entry.title.trim().is_empty() {
                entry.url.as_str()
            } else {
                entry.title.as_str()
            };
            history.children.push(
                Resource::new(
                    history_resource_id(&entry.url),
                    "browser.history-entry",
                    title,
                )
                .with_action(
                    E::Open {
                        url: entry.url.clone(),
                    }
                    .bound(),
                ),
            );
        }

        let mut bookmarks = Resource::new("browser.bookmarks", "browser.bookmarks", "Bookmarks");
        bookmarks.actions.push(E::ShowBookmarks.bound());
        // Each bookmark is an item of the document; this window adds opening
        // its page to it.
        let bookmark_rows: Vec<Resource> = self
            .bookmarks
            .entries()
            .iter()
            .take(MATERIALIZED_LIMIT)
            .map(|entry| {
                Resource::new(
                    bookmark_resource_id(&entry.id),
                    "browser.bookmark",
                    entry.title.clone(),
                )
                .with_action(
                    E::Open {
                        url: entry.url.clone(),
                    }
                    .bound(),
                )
            })
            .collect();

        let mut downloads = Resource::new("browser.downloads", "browser.downloads", "Downloads");
        downloads.actions.push(E::ShowDownloads.bound());
        downloads
            .actions
            .push(E::StartDownload { url: String::new() }.bound_asking(&["url"]));
        for entry in self
            .downloads
            .entries()
            .iter()
            .rev()
            .take(MATERIALIZED_LIMIT)
        {
            let id = entry.id.clone();
            let mut child = Resource::new(
                entry.resource_id(),
                "browser.download",
                entry.display_name(),
            )
            .with_action(E::ShowDownload { id: id.clone() }.bound());
            match entry.status {
                DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => {
                    child = child.with_action(E::CancelDownload { id }.bound());
                }
                DownloadStatus::Failed | DownloadStatus::Cancelled => {
                    child = child.with_action(E::RetryDownload { id }.bound());
                }
                DownloadStatus::Completed => {
                    child = child
                        .with_action(E::OpenDownload { id: id.clone() }.bound())
                        .with_action(E::RevealDownload { id }.bound());
                }
            }
            downloads.children.push(child);
        }

        let mut extensions =
            Resource::new("browser.extensions", "browser.extensions", "Extensions");
        extensions.actions.push(E::ShowExtensions.bound());
        // Installed add-ons are durable local profile entities even while the
        // engine's activation API is unavailable. Publish each bounded,
        // address-safe inventory item so the action directory, invocation
        // history and agents use the same identity as the extension manager.
        for extension in crate::extensions::list_installed() {
            extensions.children.push(
                Resource::new(
                    crate::extensions::resource_id(&extension.id),
                    "browser.extension",
                    extension.name,
                )
                .with_action(E::ShowExtensions.bound()),
            );
        }

        // Library is a first-class presentation resource rather than an
        // unaddressable collection of tabs.  This lets keyboard users,
        // assistive technology, global action search, and agents all use the
        // same section operations as the visible surface.
        let mut library = Resource::new("browser.library", "browser.library", "Library");
        for effect in [
            E::ShowHistory,
            E::ShowBookmarks,
            E::ShowDownloads,
            E::ShowExtensions,
        ] {
            library.actions.push(effect.bound());
        }
        library.actions.push(
            E::SetLibraryFilter {
                query: String::new(),
            }
            .bound_asking(&["query"]),
        );
        let clear_filter = E::ClearLibraryFilter.bound();
        library
            .actions
            .push(if self.library_filter.get().trim().is_empty() {
                clear_filter.unavailable("already_clear", "Library filter is already clear")
            } else {
                clear_filter
            });

        let mut settings = Resource::new("browser.settings", "browser.settings", "Settings");
        settings.actions.push(E::ShowSettings.bound());
        settings.actions.push(E::RequestSiteDataClear.bound());

        let mut browser = Resource::new(
            crate::resource::Browser::KIND,
            crate::resource::Browser::KIND,
            "Browser",
        );
        browser.actions = E::specs();
        browser.children = vec![
            page, session, history, bookmarks, downloads, extensions, library, settings,
        ];
        browser.children.extend(bookmark_rows);
        for selection in &self.service_selections {
            browser.children.push(selection.resource());
        }
        browser.children.push(
            Resource::new(
                "browser.service-sessions",
                "browser.service-sessions",
                "Accounts",
            )
            .with_action(E::ListServiceSessions.bound()),
        );
        if self.service_view.is_some()
            || self.service_pending.is_some()
            || self.service_forget.is_some()
            || self.service_observation.is_some()
        {
            browser.children.push(
                Resource::new(
                    "browser.service-status",
                    "browser.service-status",
                    "Account",
                )
                .with_action(E::ReadServiceSession.bound()),
            );
        }
        if self.person_bookmarks.is_some() {
            browser.children.push(self.personal_bookmark_resources());
        }
        if let Some(resource) = self.continuity_resource() {
            browser.children.push(resource);
        }
        browser
    }
}

impl ui::widget::controls::ActionControls for BrowserWidget {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_controls.pop_front().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
}

impl BrowserWidget {
    /// The update of `invocation`, an operation [`Self::admit`] accepted,
    /// once it settled: the page loaded, the media answered, the engine
    /// replied, or it timed out.
    pub(crate) fn poll_invocation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        self.drive_page_operation();
        if self
            .page_operation_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.page_operation_result.take();
        }
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
                .is_some_and(|deadline| ui::time::now() >= deadline)
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
                .is_some_and(|deadline| ui::time::now() >= deadline)
        {
            self.resource_navigation_invocation = None;
            self.resource_navigation_deadline = None;
            self.cancel_owned_navigation();
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

    /// Answer every effect waiting on an operation that settled.
    fn settle_replies(&mut self) {
        let waiting = std::mem::take(&mut self.pending_replies);
        for (invocation, reply) in waiting {
            let answer = match self.poll_invocation(invocation) {
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output),
                Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) => Err(error),
                Some(_) | None => {
                    self.pending_replies.push((invocation, reply));
                    continue;
                }
            };
            reply.answer(answer);
        }
    }

    /// Answer `reply` once the operation `invocation` names settles.
    pub(crate) fn answer_later(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.pending_replies.push((invocation, reply));
    }

    fn sync_frame(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.poll_service_admission();
        if let Some(engine) = self
            .service_view
            .as_mut()
            .and_then(crate::service_sessions::ServiceView::poll_engine)
        {
            self.engine = engine;
            self.composed.invalidate();
            self.bootstrap_done = false;
            self.structure_dirty = true;
        }
        if let Some(view) = self.service_view.as_mut()
            && view.poll_open()
        {
            self.structure_dirty = true;
        }
        if self.private_window_request.get() {
            self.private_window_request.set(false);
            self.private_window_notice = crate::private_window::launch(None).err();
            self.structure_dirty = true;
        }
        let paused = self.history_paused();
        if self.observed_history_paused != paused {
            self.observed_history_paused = paused;
            self.structure_dirty = true;
        }
        if !self.bootstrap_done {
            self.site.initialize(&self.engine);
            if !self.headless {
                self.engine
                    .send(WebViewCommand::SetClearOnExit(self.settings.clear_on_exit));
                if self.settings.adblock_enabled {
                    crate::blocker::spawn_filter_loader(self.engine.clone());
                    // Scriptlet-style blocking (YouTube ad-skip etc.) the
                    // declarative content filter can't express.
                    self.install_blocking_scripts();
                }
                if !self.settings.adblock_enabled {
                    self.set_adblock(false);
                }
                if !self.explicit_navigation_requested {
                    self.engine
                        .send(WebViewCommand::Navigate(self.initial_url.clone()));
                }
            }
            self.bootstrap_done = true;
        }

        self.structure_dirty |= self.extensions.sync(&self.engine, &self.current_uri);
        self.structure_dirty |= self.daily.sync(&self.engine);
        let site_was_open = self.site.open;
        self.structure_dirty |= self.site.sync(&self.engine);
        for (origin, enabled) in self.site.take_decisions() {
            self.submit(
                Browser::SET_SITE_PROTECTION.with(browser::SetSiteProtection { origin, enabled }),
            );
        }
        if self.site.open && !site_was_open {
            self.deactivate_credentials();
        }
        self.structure_dirty |= self.credentials.sync(&self.engine);
        if self.credentials.open {
            self.site.close();
        }
        self.tick_back_hold(ui::time::now());
        self.poll_continuity();
        self.follow_history();
        self.follow_personal_bookmarks();
        self.drive_engine_setup();
        self.apply_engine_events();
        self.apply_panel_events();
        self.structure_dirty |= self.sync_window_toolbar();
        self.structure_dirty |= self.run_pending_commands();
        self.apply_review_stage();
        self.drive_page_operation();
        self.structure_dirty |= self.capabilities.downloads_changed();
        self.sync_search_and_suggestions();
        // Suggestions for what was just typed in the bar go out this frame.
        self.declare_window_toolbar();

        if let Some(c) = self.composed.element_mut() {
            c.sync(state, path);
        }
        self.select_inline_completion();
        self.poll_active_pip_element();
    }

    /// Start `operation` for `invocation`: done at once with its output, or
    /// accepted, settling later through [`Self::poll_invocation`].
    ///
    /// # Errors
    /// Returns why the window cannot do it now.
    pub(crate) fn admit(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        operation: BrowserOperation,
    ) -> Result<ui::framework::Effected, String> {
        if matches!(operation, BrowserOperation::ReadServiceSession) {
            if let Some(pending) = &self.service_forget {
                return Ok(ui::framework::Effected::value(pending.report()));
            }
            if let Some(report) = &self.service_observation {
                return Ok(ui::framework::Effected::value(report.clone()));
            }
            if let Some(view) = &self.service_view {
                let mut status = view.report();
                if let Some(object) = status.as_object_mut() {
                    object.insert(
                        "destination_navigation_observed".into(),
                        serde_json::json!(view.destination_navigation_observed(&self.current_uri)),
                    );
                }
                return Ok(ui::framework::Effected::value(status));
            }
            if let Some(view) = &self.service_pending {
                return Ok(ui::framework::Effected::value(view.report()));
            }
            return Err("This window does not show a connected service".into());
        }
        if let Some(result) = self.continuity_operation(&operation) {
            return result;
        }
        if let BrowserOperation::Page(request) = operation {
            self.start_page_operation(invocation, request)?;
            return Ok(ui::framework::Effected::Pending);
        }
        if matches!(&operation, BrowserOperation::ToggleReaderView) {
            if !is_supported_web_url(&self.current_uri) {
                return Err("Reader view needs a web page".into());
            }
            let enabled = !self.reader_view;
            self.start_page_operation(invocation, PageRequest::Reader { enabled })?;
            self.reader_request = Some(enabled);
            return Ok(ui::framework::Effected::Pending);
        }
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
                return Ok(ui::framework::Effected::Done(local_output(
                    "Picture in Picture closed.",
                )));
            }
            self.begin_pip_probe();
            let request_id = self
                .active_pip_probe
                .ok_or_else(|| "Picture in Picture probe did not start".to_string())?;
            self.resource_pip_invocation = Some(invocation);
            self.resource_pip_request = Some(request_id);
            self.resource_pip_deadline = Some(std::time::Instant::now() + PIP_OPERATION_TIMEOUT);
            return Ok(ui::framework::Effected::Pending);
        }
        if !is_navigation {
            let is_history_pause_toggle =
                matches!(&operation, BrowserOperation::ToggleHistoryPause);
            let output = match &operation {
                BrowserOperation::ClearHistory => local_output("Browsing history cleared."),
                BrowserOperation::BookmarkPersonalPage { .. }
                | BrowserOperation::ImportPersonalBookmarks { .. }
                | BrowserOperation::RemovePersonalBookmark { .. }
                | BrowserOperation::RemovePersonalImportedNode { .. } => {
                    local_output("Personal bookmark library updated.")
                }
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
                BrowserOperation::OpenInNewTab { .. } => {
                    local_output("Asked the desktop for a new Browser tab beside this one.")
                }
                _ => ui::tilo_actions::ActionOutput::default(),
            };
            self.perform_browser_operation(operation)?;
            let output = if is_history_pause_toggle {
                local_output(if self.history_paused() {
                    "History paused in this window."
                } else {
                    "History resumed in this window."
                })
            } else {
                output
            };
            return Ok(ui::framework::Effected::Done(output));
        }
        if self.resource_navigation_invocation.is_some()
            || self.resource_navigation_result.is_some()
        {
            return Err("another browser navigation is already running".into());
        }
        let request = self
            .next_navigation_request
            .checked_add(1)
            .filter(|id| i32::try_from(*id).is_ok())
            .ok_or("navigation request IDs exhausted")?;
        self.next_navigation_request = request;
        self.resource_navigation_invocation = Some(invocation);
        self.resource_navigation_request = Some(request);
        self.resource_navigation_deadline = Some(std::time::Instant::now() + NAVIGATION_TIMEOUT);
        if let Err(error) = self.perform_browser_operation_owned(operation, Some(request)) {
            self.resource_navigation_invocation = None;
            self.resource_navigation_deadline = None;
            self.cancel_owned_navigation();
            return Err(error);
        }
        Ok(ui::framework::Effected::Pending)
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

    #[test]
    fn public_inventory_cannot_answer_from_the_initial_render_cache() {
        let mut widget = BrowserWidget::new_with_mode(BrowsingMode::Normal);
        widget.accept_service_catalog(Vec::new());
        let resource = widget.published();
        let inventory = resource
            .children
            .iter()
            .find(|row| row.id.0 == "browser.service-sessions")
            .unwrap();
        let action = inventory
            .actions
            .iter()
            .find(|action| action.id == "list-service-sessions")
            .unwrap();
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        assert!(matches!(
            effect_operation(&encoded.payload),
            BrowserOperation::ListServiceSessions
        ));
        assert!(
            widget
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    BrowserOperation::ListServiceSessions
                )
                .is_err()
        );
    }

    #[test]
    fn pending_revocation_and_stale_selection_remain_publicly_readable() {
        let mut widget = BrowserWidget::new_with_mode(BrowsingMode::Normal);
        for status in [
            "forgetting",
            "forget-incomplete",
            "forgotten",
            "needs-reconnect",
        ] {
            widget.service_observation = Some(
                serde_json::json!({"session_id": "selected", "status": status,
                "custody_revoked": status == "forgotten", "engine_shutdown_confirmed": status == "forgotten",
                "custody_backend_needed": status != "forgotten", "local_only": false,
                "requires_reconnect": status == "needs-reconnect"}),
            );
            let resource = widget.published();
            let row = resource
                .children
                .iter()
                .find(|row| row.id.0 == "browser.service-status")
                .unwrap();
            let action = row
                .actions
                .iter()
                .find(|action| action.id == "read-service-session")
                .unwrap();
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            let ui::framework::Effected::Done(output) = widget
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    effect_operation(&encoded.payload),
                )
                .unwrap()
            else {
                panic!("observation must be immediately readable")
            };
            let value = output.value.unwrap();
            assert_eq!(value["session_id"], "selected");
            assert_eq!(value["status"], status);
            assert_eq!(value["local_only"], false);
        }
    }

    use ui::framework::HostDelivery as _;
    use ui::tilo_actions::OperationKind;
    use ui::widget::MenuButton;

    /// What the window does for the effect an encoded invocation names.
    fn effect_operation(payload: &[u8]) -> BrowserOperation {
        use ui::tilo_resource::Effect as _;
        crate::effects::BrowserEffect::from_payload(payload)
            .expect("a browser effect")
            .operation()
    }
    use ui::webview::WebViewEngine;

    fn compose(widget: &mut BrowserWidget) {
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&doc, &root);
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
    }

    pub(super) fn activate_overflow_command(widget: &mut BrowserWidget, label: &str) {
        fn menu(widget: &mut dyn Widget) -> Option<&mut MenuButton> {
            if widget.test_id() == Some(toolbar::OVERFLOW_ID) {
                return widget.as_any_mut()?.downcast_mut::<MenuButton>();
            }
            widget.children_mut().into_iter().find_map(menu)
        }
        fn target(node: ui::a11y::AccessNode, label: &str) -> Option<String> {
            if node.name.as_deref() == Some(label) {
                return node.action_target;
            }
            node.children
                .into_iter()
                .find_map(|child| target(child, label))
        }
        let menu = menu(widget.composed.element_mut().expect("actual composition"))
            .expect("retained overflow menu");
        menu.open(true);
        let target = target(menu.accessibility().expect("exported menu"), label)
            .expect("the command has its real semantic target");
        assert!(menu.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT
        ));
    }

    fn assert_no_document_control_paths(widget: &BrowserWidget) {
        assert!(
            ui::widget::controls::control_action_bindings(widget)
                .iter()
                .all(|binding| {
                    !matches!(
                        binding.kind,
                        ui::widget::ControlBindingKind::CollaborativeDocument(_)
                    )
                })
        );
    }

    pub(super) fn assert_action_binding(
        widget: &BrowserWidget,
        control_id: &str,
        resource: &str,
        action: &str,
    ) {
        assert!(
            ui::widget::controls::control_action_bindings(widget)
                .iter()
                .any(|binding| {
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

    fn dispatch_back_event(widget: &mut BrowserWidget, event: &WindowEvent) -> bool {
        ui::testing::dispatch(widget, event)
    }

    fn mouse(button: MouseButton, state: ElementState) -> WindowEvent {
        ui::synthetic_input::mouse_input(state, button)
    }

    fn key(named: WNamedKey) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            WKey::Named(named),
            None,
            ElementState::Pressed,
        ))
    }

    #[test]
    fn delayed_address_paste_survives_layout() {
        use std::sync::{Arc, Mutex, mpsc};
        use ui::clipboard::{ClipboardBackend, ClipboardTarget};

        #[derive(Default)]
        struct Delayed {
            reply: Mutex<Option<mpsc::Sender<Option<String>>>>,
        }
        impl ClipboardBackend for Delayed {
            fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
                false
            }
            fn text(&self, _: ClipboardTarget) -> Option<String> {
                None
            }
            fn request_text(&self, _: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
                let (reply, result) = mpsc::channel();
                *self.reply.lock().unwrap() = Some(reply);
                result
            }
            fn name(&self) -> &'static str {
                "delayed-address-paste"
            }
        }
        let clipboard = Arc::new(Delayed::default());
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        let mut widget = BrowserWidget::with_engine(TestEngine::new(), true);
        let (tree, node) =
            super::tests::layout_keeping_tree(&mut widget, 900.0, 700.0, &Theme::dark());
        assert!(widget.focus_address_input());
        let mut event = key(WNamedKey::Enter);
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.logical_key = WKey::Character("v".into());
        }
        let mut document = AutoCommit::new();
        let handled = ui::testing::Dispatch::new()
            .doc(&mut document)
            .layout(&tree, node)
            .modifiers(winit::keyboard::ModifiersState::CONTROL)
            .send(&mut widget, &event)
            .handled;
        assert!(handled);
        let reply = clipboard
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("native paste requested");
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        reply
            .send(Some("https://example.test/pasted".into()))
            .expect("layout retains the pending paste");
        widget.sync(&document, &automerge::ROOT);
        assert_eq!(
            widget.address.with(Clone::clone),
            "https://example.test/pasted"
        );
    }

    /// Find-in-page focus handoff, DPI through find reflow and the daily shortcut capture set.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn find_and_daily_shortcuts_route_focus_keys_and_dpi() {
        // Phase `find_focus_moves_to_search_then_returns_to_page_and_survives_rebuild`.
        {
            let mut widget = BrowserWidget::new_headless();
            compose(&mut widget);
            assert!(widget.focus_control("browser.surface"));
            compose(&mut widget);
            assert!(widget.control_focused("browser.surface"));
            widget
                .daily
                .act(crate::daily_controls::Action::Open, &widget.engine);
            compose(&mut widget);
            assert!(widget.control_focused(crate::daily_controls::FIND_INPUT));
            widget
                .daily
                .act(crate::daily_controls::Action::Close, &widget.engine);
            compose(&mut widget);
            assert!(widget.control_focused("browser.surface"));
        }

        // Phase `rebuilt_web_content_keeps_window_dpi_through_find_reflow`.
        {
            let engine = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            ui::testing::Dispatch::new()
                .scale_factor(2.0)
                .send(&mut widget, &WindowEvent::Focused(true));
            super::tests::layout_at(&mut widget, 800.0, 600.0);
            widget
                .daily
                .act(crate::daily_controls::Action::Open, &widget.engine);
            super::tests::layout_at(&mut widget, 800.0, 600.0);
            widget
                .daily
                .act(crate::daily_controls::Action::Close, &widget.engine);
            super::tests::layout_at(&mut widget, 800.0, 600.0);
            let sizes: Vec<_> = engine
                .commands()
                .into_iter()
                .filter_map(|command| match command {
                    WebViewCommand::Resize {
                        width,
                        height,
                        scale_factor,
                    } => Some((width, height, scale_factor)),
                    _ => None,
                })
                .collect();
            assert!(sizes.len() >= 3, "{sizes:?}");
            assert!(
                sizes
                    .iter()
                    .all(|&(width, _, scale)| width > 1000 && (scale - 2.0).abs() < f32::EPSILON),
                "{sizes:?}"
            );
            assert!(
                sizes[1].1 < sizes[0].1,
                "Find must shrink the page viewport"
            );
            assert_eq!(sizes.first(), sizes.last());
        }

        // Phase `find_handoff_restores_focus_before_first_character_dispatch`.
        {
            let capture = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(capture.clone(), true);
            compose(&mut widget);
            widget
                .daily
                .act(crate::daily_controls::Action::Open, &widget.engine);
            let mut tree = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let node = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            // The host restores its previous focus after rebuilding. The next
            // event must reapply the pending find focus before dispatching text.
            widget.focus_control("browser.surface");
            let mut doc = AutoCommit::new();
            let mut target = None;
            for letter in ["N", "e", "e", "d", "l", "e"] {
                let mut event = key(WNamedKey::Enter);
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = WKey::Character(letter.into());
                    event.text = Some(letter.into());
                }
                target = ui::testing::Dispatch::new()
                    .doc(&mut doc)
                    .layout(&tree, node)
                    .focus_target(target)
                    .send(&mut widget, &event)
                    .focus_target;
                widget.daily.sync(&widget.engine);
            }
            widget.daily.sync(&widget.engine);
            assert!(widget.control_focused(crate::daily_controls::FIND_INPUT));
            assert!(
                capture.commands().iter().any(|command| matches!(command,
                WebViewCommand::Find { text, .. } if text == "Needle")),
                "{:?}",
                capture.commands()
            );
        }

        // Phase `daily_shortcuts_capture_find_zoom_bookmark_but_leave_plain_typing`.
        {
            let mut widget = BrowserWidget::new_headless();
            for letter in ["f", "+", "-", "0", "d"] {
                let mut event = key(WNamedKey::Enter);
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = WKey::Character(letter.into());
                }
                assert!(
                    !widget.handle_browser_shortcut(&event, winit::event::Modifiers::default())
                );
                assert!(
                    widget.handle_browser_shortcut(
                        &event,
                        (if cfg!(target_os = "macos") {
                            winit::keyboard::ModifiersState::SUPER
                        } else {
                            winit::keyboard::ModifiersState::CONTROL
                        })
                        .into()
                    ),
                    "{letter}"
                );
            }
        }
    }

    #[test]
    fn owned_navigation_rejects_unrelated_completion_and_preserves_redirect_output() {
        use ui::tilo_resource::Effect as _;

        struct Events(
            std::sync::Mutex<Vec<WebViewEvent>>,
            std::sync::Arc<TestEngine>,
        );
        impl WebViewEngine for Events {
            fn send(&self, command: WebViewCommand) {
                self.1.send(command);
            }
            fn navigate_owned(
                &self,
                request_id: u32,
                command: NavigationCommand,
            ) -> Result<(), String> {
                self.1.navigate_owned(request_id, command)
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.1.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.0.lock().unwrap())
            }
        }
        let engine =
            std::sync::Arc::new(Events(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
        let mut widget = BrowserWidget::with_engine(engine.clone(), false);
        let invocation = ui::tilo_actions::InvocationId::new();
        assert!(matches!(
            widget
                .admit(
                    invocation,
                    BrowserOperation::Navigate {
                        url: "https://request.test/".into()
                    }
                )
                .unwrap(),
            ui::framework::Effected::Pending
        ));
        engine.0.lock().unwrap().extend([
            WebViewEvent::UriChanged("about:blank".into()),
            WebViewEvent::LoadFinished,
            WebViewEvent::LoadFailed {
                uri: "about:blank".into(),
                error: "stale startup".into(),
            },
            WebViewEvent::NavigationOutcome {
                request_id: 999,
                uri: "about:blank".into(),
                error: None,
            },
        ]);
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&doc, &root);
        assert!(widget.poll_invocation(invocation).is_none());
        assert!(
            !engine
                .1
                .commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::Navigate(_))),
            "late default initialization must not supersede owned navigation"
        );
        let request_id = widget.resource_navigation_request.unwrap();
        engine
            .0
            .lock()
            .unwrap()
            .push(WebViewEvent::NavigationOutcome {
                request_id,
                uri: "https://redirect.test/".into(),
                error: None,
            });
        widget.apply_engine_events();
        let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            widget.poll_invocation(invocation)
        else {
            panic!("matching owned completion must succeed");
        };
        assert_eq!(
            output.value,
            Some(serde_json::json!("https://redirect.test/"))
        );
        let contract = crate::effects::BrowserEffect::Navigate {
            url: "https://request.test/".into(),
        }
        .bound()
        .contract;
        assert!(matches!(
            contract.execution,
            ui::tilo_actions::ExecutionMode::Background { cancellable: false }
        ));
        assert!(contract.validate_declaration().is_empty());
        contract.result.validate(&output).unwrap();
        let failed = ui::tilo_actions::InvocationId::new();
        widget.admit(failed, BrowserOperation::Reload).unwrap();
        let request_id = widget.resource_navigation_request.unwrap();
        engine
            .0
            .lock()
            .unwrap()
            .push(WebViewEvent::NavigationOutcome {
                request_id,
                uri: "https://redirect.test/".into(),
                error: Some("ERR_CONNECTION_REFUSED".into()),
            });
        widget.apply_engine_events();
        let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
            widget.poll_invocation(failed)
        else {
            panic!("matching owned failure must fail");
        };
        assert_eq!(error.code, "browser.navigation_failed");
        assert_eq!(error.message, "ERR_CONNECTION_REFUSED");
    }

    /// Loading state: stop contract, animation scheduling, and when loading starts and ends.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn loading_state_starts_stops_and_animates_only_while_live() {
        // Phase `stop_contract_completes_and_queued_progress_cannot_restart_loading`.
        {
            struct QueuedEngine {
                events: std::sync::Mutex<Vec<WebViewEvent>>,
                capture: std::sync::Arc<TestEngine>,
            }
            impl WebViewEngine for QueuedEngine {
                fn navigate_owned(
                    &self,
                    request_id: u32,
                    command: NavigationCommand,
                ) -> Result<(), String> {
                    self.capture.navigate_owned(request_id, command)
                }
                fn send(&self, command: WebViewCommand) {
                    self.capture.send(command);
                }
                fn frames(&self) -> ui::webview::FrameSlot {
                    self.capture.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
            }
            let capture = TestEngine::new();
            let engine = std::sync::Arc::new(QueuedEngine {
                events: std::sync::Mutex::new(Vec::new()),
                capture: capture.clone(),
            });
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            let invocation = ui::tilo_actions::InvocationId::new();
            assert!(matches!(
                widget
                    .admit(
                        invocation,
                        BrowserOperation::Navigate {
                            url: "https://stop.test/".into()
                        }
                    )
                    .unwrap(),
                ui::framework::Effected::Pending
            ));
            widget.set_loading_progress(0.2);
            let resources = [widget.published()];
            let stop = resources[0]
                .children
                .iter()
                .find(|r| r.id.0 == "browser.page")
                .unwrap()
                .actions
                .iter()
                .find(|a| a.id == "stop")
                .unwrap();
            let encoded = stop.build_json(&serde_json::Map::new()).unwrap();
            let result = widget
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    effect_operation(&encoded.payload),
                )
                .map(Some)
                .unwrap();
            let Some(ui::framework::Effected::Done(output)) = result else {
                panic!("Stop must complete immediately");
            };
            assert!(stop.contract.result.validate(&output).is_ok());
            assert!(matches!(
                widget.resource_navigation_result,
                Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
            ));
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::LoadProgress(0.4));
            widget.apply_engine_events();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            assert!(capture.commands().contains(&WebViewCommand::StopLoading));
            widget
                .perform_browser_operation(BrowserOperation::Reload)
                .unwrap();
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::LoadProgress(0.6));
            widget.apply_engine_events();
            assert!((widget.progress - 0.6).abs() < f64::EPSILON);

            // A failed request can leave Chromium on its previous document.
            // Late progress and that document's completion must not conceal
            // the failed address or restart the spinner; retry owns the URL.
            let failed_uri = "http://127.0.0.1:9/";
            engine.events.lock().unwrap().extend([
                WebViewEvent::LoadFailed {
                    uri: failed_uri.into(),
                    error: "ERR_UNSAFE_PORT".into(),
                },
                WebViewEvent::LoadProgress(0.3),
                WebViewEvent::LoadFinishedAt {
                    uri: "https://stop.test/".into(),
                },
                WebViewEvent::LoadFinished,
            ]);
            widget.apply_engine_events();
            assert_eq!(widget.address.get(), failed_uri);
            assert_eq!(widget.current_uri, failed_uri);
            assert_eq!(widget.load_error.as_deref(), Some("ERR_UNSAFE_PORT"));
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            widget
                .perform_browser_operation(BrowserOperation::Reload)
                .unwrap();
            assert!(
                capture
                    .commands()
                    .contains(&WebViewCommand::Navigate(failed_uri.into()))
            );
            assert!(widget.load_error.is_none());
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::LoadProgress(0.4));
            widget.apply_engine_events();
            assert!((widget.progress - 0.4).abs() < f64::EPSILON);
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::LoadFailed {
                    uri: "file:///outside-workspace.html".into(),
                    error: "blocked".into(),
                });
            widget.apply_engine_events();
            widget
                .perform_browser_operation(BrowserOperation::Reload)
                .unwrap();
            assert_eq!(capture.commands().last(), Some(&WebViewCommand::Reload));
        }

        // Phase `stopped_and_reduced_motion_loading_do_not_schedule_animation`.
        {
            let engine = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            widget
                .perform_browser_operation(BrowserOperation::Navigate {
                    url: "https://a.test".into(),
                })
                .unwrap();
            assert!(widget.needs_animation_frame(), "the loading line sweeps");
            widget.set_motion_reduced(true);
            assert!(
                !widget.needs_animation_frame(),
                "under reduced motion the loading line holds still"
            );
            widget
                .perform_browser_operation(BrowserOperation::Stop)
                .unwrap();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            assert!(engine.commands().contains(&WebViewCommand::StopLoading));
            assert!(widget.take_structure_dirty());
        }

        // Phase `navigation_starts_loading_before_engine_callbacks`.
        {
            let engine = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(engine, true);
            widget
                .perform_browser_operation(BrowserOperation::Navigate {
                    url: "https://a.test".into(),
                })
                .unwrap();
            assert!(
                widget.progress < 1.0,
                "navigation must paint loading before callbacks"
            );
            assert!(
                widget.needs_animation_frame(),
                "loading indicator animates immediately"
            );
            widget.set_loading_progress(0.4);
            assert!(
                !widget.needs_animation_frame(),
                "a load with reported progress repaints only when progress moves"
            );
            widget.apply_engine_events();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            assert!(widget.loading_face().is_none());
            assert!(!widget.needs_animation_frame());
        }

        // Phase `fresh_live_surface_starts_loading_and_failure_stops_it`.
        {
            let engine = ui::webview::chromium::UnavailableEngine::new("offline".into());
            let mut widget = BrowserWidget::with_engine(engine, false);
            assert!(
                widget.progress < 1.0,
                "new tab is visibly loading before bootstrap"
            );
            widget
                .engine
                .send(WebViewCommand::Navigate("https://a.test".into()));
            widget.apply_engine_events();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            assert!(
                !widget
                    .build_composition(&Theme::dark())
                    .needs_animation_frame()
            );
        }
    }

    /// Every hosted affordance is a canonical action; presentation stays peer-local.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn hosted_browser_affordances_are_canonical_peer_local_actions() {
        // Phase `composed_download_save_action_reaches_native_callback`.
        {
            let capture = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(capture.clone(), true);
            widget
                .capabilities
                .apply(WebViewEvent::NativeDownloadRequested {
                    id: 1,
                    url: "https://test.example/file".into(),
                    name: "file.txt".into(),
                });
            compose(&mut widget);
            let result = widget
                .start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.decision.false.1.true").unwrap(),
                    &mut AutoCommit::new(),
                    &automerge::ROOT,
                )
                .unwrap();
            assert!(result.is_some());
            assert!(
                capture
                    .commands()
                    .contains(&WebViewCommand::DownloadDecision { id: 1, allow: true })
            );
        }

        // Phase `presentation_operations_are_peer_local_and_do_not_mutate_the_document`.
        {
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

        // Phase `every_hosted_browser_affordance_uses_a_canonical_action`.
        {
            let mut page = BrowserWidget::new_headless();
            compose(&mut page);
            assert_no_document_control_paths(&page);
            for (control, resource, action) in [
                ("browser.back", "browser.page", "back"),
                ("browser.forward", "browser.page", "forward"),
                ("browser.reload", "browser.page", "reload"),
                ("browser.library", "browser.history", "show-history"),
            ] {
                assert_action_binding(&page, control, resource, action);
            }
            page.debug_show_library("history");
            compose(&mut page);
            assert_action_binding(
                &page,
                "browser.library.history-pause",
                "browser.session",
                "toggle-pause",
            );
            // The overflow menu's commands queue the same canonical actions.
            for (label, resource, action) in [
                ("Pause history", "browser.session", "toggle-pause"),
                ("Picture in Picture", "browser.page", "picture-in-picture"),
                ("Settings", "browser.settings", "show-settings"),
            ] {
                let mut page = BrowserWidget::new_headless();
                compose(&mut page);
                activate_overflow_command(&mut page, label);
                assert!(page.run_pending_commands());
                assert!(
                    page.pending_controls.iter().any(|invocation| {
                        invocation.address.resource == resource
                            && invocation.address.action == action
                    }),
                    "{resource}/{action} reaches the action host from the overflow menu"
                );
            }

            let mut library = BrowserWidget::new_headless();
            library.debug_show_library("history");
            library
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "history".into(),
                })
                .unwrap();
            compose(&mut library);
            assert_no_document_control_paths(&library);
            assert_action_binding(
                &library,
                "browser.library.clear-filter",
                "browser.library",
                "clear-library-filter",
            );

            let mut settings = BrowserWidget::new_headless();
            settings.show_settings = true;
            compose(&mut settings);
            assert_no_document_control_paths(&settings);
            for (control, resource, action) in [
                (
                    "settings.close",
                    crate::resource::Browser::KIND,
                    "show-page",
                ),
                ("settings.clear_history", "browser.history", "clear-history"),
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

    /// Back/forward history menus request real session history, split entries and drop stale replies.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn session_history_menus_request_real_entries_and_ignore_stale_replies() {
        // Phase `hold_back_requests_real_session_history_and_release_does_not_navigate`.
        {
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test", "https://c.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.cursor = widget.find_rect("browser.back").unwrap().center();
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed)
            ));
            assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
            widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(451));
            widget.apply_engine_events();
            assert!(widget.back_menu.is_open());
            assert_eq!(
                widget
                    .back_entries
                    .iter()
                    .map(|e| e.url.as_str())
                    .collect::<Vec<_>>(),
                ["https://b.test", "https://a.test"]
            );
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Released)
            ));
            assert!(widget.pending_controls.is_empty());
            assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
            let accessible = widget.back_menu.accessibility().unwrap();
            assert_eq!(accessible.children.len(), 2);
            assert!(dispatch_back_event(&mut widget, &key(WNamedKey::ArrowDown)));
            assert!(dispatch_back_event(&mut widget, &key(WNamedKey::ArrowDown)));
            assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Enter)));
            assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
            engine.send(WebViewCommand::GoForward);
            assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
        }

        // Phase `compact_forward_button_exposes_directional_history`.
        {
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            engine.send(WebViewCommand::GoBack);
            let mut widget = BrowserWidget::with_engine(engine, true);
            widget.debug_set_viewport_width(360);
            super::tests::layout_at(&mut widget, 360.0, 700.0);
            widget.apply_engine_events();
            let forward = widget
                .find_rect("browser.forward")
                .expect("compact Forward must remain visible");
            assert!(forward.width() >= 30.0 && forward.x0 >= 0.0 && forward.x1 <= 360.0);
            widget.cursor = forward.center();
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Right, ElementState::Pressed)
            ));
            widget.apply_engine_events();
            assert!(widget.back_menu.is_open());
            assert_eq!(
                widget
                    .back_entries
                    .iter()
                    .map(|entry| entry.url.as_str())
                    .collect::<Vec<_>>(),
                ["https://b.test"]
            );
        }

        // Phase `current_entry_splits_nearest_back_and_forward_history_and_forward_hold_opens_once`.
        {
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test", "https://c.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            engine.send(WebViewCommand::GoBack);
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.apply_engine_events();
            widget.cursor = widget.find_rect("browser.back").unwrap().center();
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Right, ElementState::Pressed)
            ));
            widget.apply_engine_events();
            assert_eq!(
                widget
                    .back_entries
                    .iter()
                    .map(|entry| (entry.id, entry.url.as_str()))
                    .collect::<Vec<_>>(),
                [(1, "https://a.test")]
            );
            assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Escape)));
            widget.cursor = widget.find_rect("browser.forward").unwrap().center();

            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed)
            ));
            widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(451));
            widget.apply_engine_events();

            assert!(widget.back_menu.is_open());
            assert_eq!(
                widget
                    .back_entries
                    .iter()
                    .map(|entry| (entry.id, entry.url.as_str()))
                    .collect::<Vec<_>>(),
                [(3, "https://c.test")]
            );
            let request_count = engine
                .commands()
                .iter()
                .filter(|command| {
                    matches!(command, WebViewCommand::RequestNavigationHistory { .. })
                })
                .count();
            widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(900));
            assert_eq!(
                engine
                    .commands()
                    .iter()
                    .filter(|command| matches!(
                        command,
                        WebViewCommand::RequestNavigationHistory { .. }
                    ))
                    .count(),
                request_count
            );
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Released)
            ));
            assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
        }

        // Phase `stale_history_reply_after_direction_or_page_change_is_ignored`.
        {
            struct DelayedEngine {
                events: std::sync::Mutex<Vec<WebViewEvent>>,
                capture: std::sync::Arc<TestEngine>,
            }
            impl WebViewEngine for DelayedEngine {
                fn send(&self, command: WebViewCommand) {
                    self.capture.send(command);
                }
                fn frames(&self) -> ui::webview::FrameSlot {
                    self.capture.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
            }

            let capture = TestEngine::new();
            let engine = std::sync::Arc::new(DelayedEngine {
                events: std::sync::Mutex::new(Vec::new()),
                capture,
            });
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.can_go_back = true;
            widget.can_go_forward = true;
            widget.current_uri = "https://b.test".into();

            widget.request_navigation_history(ui::webview::NavigationDirection::Back);
            widget.request_navigation_history(ui::webview::NavigationDirection::Forward);
            let stale_direction_request = widget.history_request;
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::NavigationHistory {
                    request_id: stale_direction_request,
                    direction: ui::webview::NavigationDirection::Back,
                    entries: vec![ui::webview::NavigationEntry {
                        id: 11,
                        url: "https://a.test".into(),
                        title: "A".into(),
                    }],
                });
            widget.apply_engine_events();
            assert!(!widget.back_menu.is_open());

            let stale_page_request = widget.history_request;
            engine.events.lock().unwrap().extend([
                WebViewEvent::UriChanged("https://new.test".into()),
                WebViewEvent::NavigationHistory {
                    request_id: stale_page_request,
                    direction: ui::webview::NavigationDirection::Forward,
                    entries: vec![ui::webview::NavigationEntry {
                        id: 33,
                        url: "https://c.test".into(),
                        title: "C".into(),
                    }],
                },
            ]);
            widget.apply_engine_events();
            assert!(!widget.back_menu.is_open());
        }
    }

    /// Media generation, fullscreen and renderer-loss events.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_fullscreen_and_renderer_events_drive_tab_state() {
        // Phase `stale_media_event_from_same_url_reload_is_ignored_by_generation`.
        {
            struct EventEngine {
                events: std::sync::Mutex<Vec<WebViewEvent>>,
                capture: std::sync::Arc<TestEngine>,
            }
            impl WebViewEngine for EventEngine {
                fn send(&self, command: WebViewCommand) {
                    self.capture.send(command);
                }
                fn frames(&self) -> ui::webview::FrameSlot {
                    self.capture.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
            }
            let engine = std::sync::Arc::new(EventEngine {
                events: std::sync::Mutex::new(vec![
                    WebViewEvent::UriChanged("https://media.test/".into()),
                    WebViewEvent::MediaPlayingChanged {
                        page_url: "https://media.test/".into(),
                        document_generation: 8,
                        playing: true,
                    },
                    WebViewEvent::MediaPlayingChanged {
                        page_url: "https://media.test/".into(),
                        document_generation: 7,
                        playing: false,
                    },
                ]),
                capture: TestEngine::new(),
            });
            let mut widget = BrowserWidget::with_engine(engine, true);
            widget.apply_engine_events();
            assert!(widget.media_playing);
            assert_eq!(widget.media_document_generation, Some(8));
        }

        // Phase `fullscreen_events_and_renderer_loss_drive_tab_state`.
        {
            struct FullscreenEngine {
                events: std::sync::Mutex<Vec<WebViewEvent>>,
                capture: std::sync::Arc<TestEngine>,
            }
            impl WebViewEngine for FullscreenEngine {
                fn send(&self, command: WebViewCommand) {
                    self.capture.send(command);
                }
                fn frames(&self) -> ui::webview::FrameSlot {
                    self.capture.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
            }
            let capture = TestEngine::new();
            let engine = std::sync::Arc::new(FullscreenEngine {
                events: std::sync::Mutex::new(vec![WebViewEvent::FullscreenChanged(true)]),
                capture: capture,
            });
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);

            widget.apply_engine_events();
            assert!(widget.content_fullscreen());
            assert!(widget.take_structure_dirty());

            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::RendererTerminated);
            widget.apply_engine_events();
            assert!(!widget.content_fullscreen());
            assert!(widget.take_structure_dirty());

            engine.events.lock().unwrap().extend([
                WebViewEvent::FullscreenChanged(true),
                WebViewEvent::FullscreenChanged(false),
            ]);
            widget.apply_engine_events();
            assert!(!widget.content_fullscreen());
        }

        // Phase `exiting_content_fullscreen_is_idempotent_and_routes_native_command_once`.
        {
            let engine = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            widget.content_fullscreen = true;

            assert!(widget.exit_content_fullscreen());
            assert!(!widget.exit_content_fullscreen());
            assert!(!widget.content_fullscreen());
            assert_eq!(
                engine
                    .commands()
                    .iter()
                    .filter(|command| matches!(command, WebViewCommand::ExitFullscreen))
                    .count(),
                1
            );
        }
    }

    /// Back/forward click, cancel, keyboard and accessible activation paths.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn back_forward_controls_open_session_entries_without_accidental_navigation() {
        // Phase `back_click_and_cancel_route_without_accidental_navigation`.
        {
            let engine = TestEngine::new();
            engine.send(WebViewCommand::Navigate("https://a.test".into()));
            engine.send(WebViewCommand::Navigate("https://b.test".into()));
            let mut widget = BrowserWidget::with_engine(engine, true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.cursor = widget.find_rect("browser.back").unwrap().center();
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed),
            );
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Released),
            );
            assert_eq!(widget.pending_controls.len(), 1);
            widget.pending_controls.clear();
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed),
            );
            dispatch_back_event(&mut widget, &WindowEvent::Focused(false));
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Released),
            );
            assert!(widget.pending_controls.is_empty());
            assert!(!widget.navigation_hold.waiting());
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Right, ElementState::Pressed),
            );
            widget.apply_engine_events();
            assert!(widget.back_menu.is_open());
            assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Escape)));
            assert!(!widget.back_menu.is_open());
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Right, ElementState::Pressed),
            );
            widget.apply_engine_events();
            assert!(widget.back_menu.is_open());
            widget.cursor = Point::new(850.0, 650.0);
            dispatch_back_event(
                &mut widget,
                &ui::synthetic_input::cursor_moved(850.0, 650.0),
            );
            assert!(dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed)
            ));
            assert!(!widget.back_menu.is_open());
        }

        // Phase `history_keyboard_open_and_accessible_activation_use_session_entry`.
        {
            fn focus_back(widget: &mut dyn Widget) {
                widget.set_focus(widget.test_id() == Some("browser.back"));
                for child in widget.children_mut() {
                    focus_back(child);
                }
            }
            let engine = TestEngine::new();
            engine.send(WebViewCommand::Navigate("https://a.test".into()));
            engine.send(WebViewCommand::Navigate("https://b.test".into()));
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            focus_back(&mut widget);
            assert!(dispatch_back_event(
                &mut widget,
                &key(WNamedKey::ContextMenu)
            ));
            widget.apply_engine_events();
            assert!(widget.back_menu.is_open());
            let node = widget.accessibility().unwrap();
            let target = node.children[0].children[0].action_target.clone().unwrap();
            assert!(widget.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT
            ));
            assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
            assert!(!widget.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT
            ));
        }

        // Phase `forward_history_keyboard_open_and_accessible_activation_use_session_entry`.
        {
            fn focus_forward(widget: &mut dyn Widget) {
                widget.set_focus(widget.test_id() == Some("browser.forward"));
                for child in widget.children_mut() {
                    focus_forward(child);
                }
            }
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test", "https://c.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            engine.send(WebViewCommand::GoBack);
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.apply_engine_events();
            focus_forward(&mut widget);

            assert!(dispatch_back_event(
                &mut widget,
                &key(WNamedKey::ContextMenu)
            ));
            widget.apply_engine_events();
            let node = widget.accessibility().unwrap();
            let target = node.children[0].children[0].action_target.clone().unwrap();
            assert!(target.contains(":forward:"));
            assert!(widget.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT
            ));
            assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
        }

        // Phase `missing_release_after_window_exit_does_not_capture_the_next_click`.
        {
            let engine = TestEngine::new();
            engine.send(WebViewCommand::Navigate("https://a.test".into()));
            engine.send(WebViewCommand::Navigate("https://b.test".into()));
            let mut widget = BrowserWidget::with_engine(engine, true);
            super::tests::layout_at(&mut widget, 900.0, 700.0);
            widget.cursor = widget.find_rect("browser.back").unwrap().center();
            dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Pressed),
            );
            dispatch_back_event(&mut widget, &WindowEvent::Focused(false));
            widget.cursor = Point::new(800.0, 600.0);
            assert!(!dispatch_back_event(
                &mut widget,
                &mouse(MouseButton::Left, ElementState::Released)
            ));
        }
    }

    /// Library open and filter controls.
    #[test]
    fn library_controls_are_bound_canonical_actions_with_stable_addresses() {
        // Phase `library_open_controls_keep_stable_resource_addresses_after_reorder`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget
                .history
                .record_at("https://first.example", "First", 1);
            widget
                .history
                .record_at("https://second.example", "Second", 2);
            widget.show_library = true;
            compose(&mut widget);
            let bindings = ui::widget::controls::control_action_bindings(&widget);
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

        // Phase `library_search_is_the_platform_search_scoped_to_the_section`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_library("bookmarks");
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "rust".into(),
                })
                .unwrap();
            compose(&mut widget);
            assert_action_binding(
                &widget,
                "browser.library.clear-filter",
                "browser.library",
                "clear-library-filter",
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
    use ui::framework::HostDelivery as _;
    use ui::tilo_actions::{ActionSpec, OperationKind};
    use ui::webview::PageKey;

    /// What the window does for the effect an encoded invocation names.
    fn effect_operation(payload: &[u8]) -> BrowserOperation {
        use ui::tilo_resource::Effect as _;
        crate::effects::BrowserEffect::from_payload(payload)
            .expect("a browser effect")
            .operation()
    }
    use std::sync::Arc;
    use ui::webview::WebViewEngine;

    fn synced(widget: &mut BrowserWidget) -> (AutoCommit, ObjId) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&state, &root);
        (state, root)
    }

    /// What a scripted host answers, one look at a time, and the fresh
    /// instances it was asked to start.
    type Looks = std::collections::VecDeque<Result<InstallStatus, String>>;
    type Relaunches = Arc<std::sync::Mutex<Vec<Option<String>>>>;
    static HOST: std::sync::Mutex<Option<(Looks, Relaunches)>> = std::sync::Mutex::new(None);
    use ui::webview::chromium::install::{EngineInstaller, InstallPhase, InstallStatus};

    struct ScriptedHost(Looks, Relaunches);

    impl EngineInstaller for ScriptedHost {
        fn status(&mut self) -> Result<InstallStatus, String> {
            self.0.pop_front().unwrap_or(Ok(InstallStatus::Installed))
        }
        fn relaunch(&mut self, uri: Option<&str>) -> Result<(), String> {
            self.1.lock().unwrap().push(uri.map(str::to_owned));
            Ok(())
        }
    }

    fn scripted_host() -> Option<Box<dyn EngineInstaller>> {
        let (looks, relaunches) = HOST.lock().unwrap().take()?;
        Some(Box::new(ScriptedHost(looks, relaunches)))
    }

    fn script(looks: Looks) -> Relaunches {
        let relaunches = Relaunches::default();
        *HOST.lock().unwrap() = Some((looks, Arc::clone(&relaunches)));
        relaunches
    }

    /// Frames until the setup shows `wanted`, or the test gives up.
    fn until_setup(
        widget: &mut BrowserWidget,
        wanted: impl Fn(&crate::engine_setup::Setup) -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline {
            let _ = synced(widget);
            if widget.engine_setup_shown.as_ref().is_some_and(&wanted) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("setup stuck at {:?}", widget.engine_setup_shown);
    }

    /// First use without the engine: the Browser asks the host for it,
    /// shows the progress, and reopens once it is installed. A failure
    /// offers a retry that asks again. Nothing here is a message to read or
    /// a command to run.
    #[test]
    fn a_missing_engine_is_installed_on_first_use_with_progress() {
        use crate::engine_setup::Setup;
        // One test owns the scripted host: statics are process-wide.
        let relaunches = script(
            [
                Ok(InstallStatus::Installing {
                    phase: InstallPhase::Preparing,
                    received: 0,
                    total: None,
                }),
                Ok(InstallStatus::Installing {
                    phase: InstallPhase::Downloading,
                    received: 40_000_000,
                    total: Some(80_000_000),
                }),
                Ok(InstallStatus::Installed),
            ]
            .into(),
        );
        let mut w = BrowserWidget::with_engine(
            unavailable_engine(BrowsingMode::Normal, EngineProblem::NotInstalled),
            false,
        );
        w.engine_problem = Some(EngineProblem::NotInstalled);
        w.engine_installer = scripted_host;
        w.initial_url = "https://tilo.so/".into();

        // The first frame starts the install without anyone pressing a button.
        let _ = synced(&mut w);
        assert!(w.engine_setup.is_some(), "first use asks the host");

        until_setup(&mut w, |setup| {
            matches!(
                setup,
                Setup::Fetching {
                    phase: InstallPhase::Downloading,
                    received: 40_000_000,
                    ..
                }
            )
        });
        assert_eq!(w.engine_setup_shown.as_ref().unwrap().fraction(), Some(0.5));
        // The page area is the toolkit's progress state.
        layout_at(&mut w, 900.0, 600.0);

        until_setup(&mut w, |setup| *setup == Setup::Reopened);
        assert_eq!(
            *relaunches.lock().unwrap(),
            [Some("https://tilo.so/".to_owned())],
            "a fresh Browser starts on the page being opened, with the engine mounted"
        );
        layout_at(&mut w, 900.0, 600.0);

        // A failed install says so and the one button asks again.
        let relaunches = script([Err("offline".to_owned())].into());
        let mut w = BrowserWidget::with_engine(
            unavailable_engine(BrowsingMode::Normal, EngineProblem::NotInstalled),
            false,
        );
        w.engine_problem = Some(EngineProblem::NotInstalled);
        w.engine_installer = scripted_host;
        until_setup(
            &mut w,
            |setup| matches!(setup, Setup::Failed(message) if message == "offline"),
        );
        layout_at(&mut w, 900.0, 600.0);
        let _retry = script([Ok(InstallStatus::Installed)].into());
        w.perform_browser_operation(BrowserOperation::InstallEngine)
            .expect("retrying is the page's install action");
        until_setup(&mut w, |setup| *setup == Setup::Reopened);
        assert!(relaunches.lock().unwrap().is_empty());
    }

    /// Page actions, as an agent reaches them: published on `browser.page`,
    /// admitted through the resource operation lane, answered by the engine
    /// and reported through the invocation's terminal update.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn page_actions_read_and_operate_the_page_through_invocations() {
        fn page_on(url: &str) -> (Arc<TestEngine>, BrowserWidget, AutoCommit, ObjId) {
            let engine = TestEngine::new();
            engine.send(WebViewCommand::Navigate(url.into()));
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            let (doc, root) = synced(&mut widget);
            (engine, widget, doc, root)
        }
        fn page_action(widget: &BrowserWidget, doc: &AutoCommit, id: &str) -> ActionSpec {
            widget
                .published()
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.page")
                .unwrap()
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap()
                .clone()
        }
        fn start(
            widget: &mut BrowserWidget,
            doc: &mut AutoCommit,
            root: &ObjId,
            id: &str,
            args: &serde_json::Value,
        ) -> Result<ui::tilo_actions::InvocationId, String> {
            let action = page_action(widget, doc, id);
            let encoded = action
                .build_json(args.as_object().ok_or("arguments are an object")?)
                .map_err(|error| error.to_string())?;
            let invocation = ui::tilo_actions::InvocationId::new();
            let _ = (doc, root);
            match widget
                .admit(invocation, effect_operation(&encoded.payload))
                .map(Some)?
            {
                Some(ui::framework::Effected::Pending) => Ok(invocation),
                other => Err(format!("a page action must be accepted, got {other:?}")),
            }
        }
        fn page_requests(engine: &TestEngine) -> Vec<PageRequest> {
            engine
                .commands()
                .into_iter()
                .filter_map(|command| match command {
                    WebViewCommand::Page { request, .. } => Some(request),
                    _ => None,
                })
                .collect()
        }

        // Phase `page_actions_publish_programmatic_write_contracts`.
        {
            let (_, widget, doc, _) = page_on("https://shop.test/");
            for id in [
                "read-page",
                "click-page",
                "fill-page",
                "select-page",
                "press-page",
                "submit-page",
                "fill-page-secret",
                "read-page-changes",
                "read-page-element-at",
                "wait-page-settled",
                "read-page-console",
                "read-page-network",
            ] {
                let action = page_action(&widget, &doc, id);
                assert!(action.contract.validate_declaration().is_empty(), "{id}");
                assert!(action.contract.availability.is_available(), "{id}");
                assert!(action.contract.is_programmatic(), "{id}");
                assert!(
                    action
                        .contract
                        .tags
                        .iter()
                        .any(|tag| tag == ui::tilo_actions::UNTRUSTED_CONTENT_TAG),
                    "{id} answers with page content: data, never instructions"
                );
                assert_eq!(
                    action.contract.kind,
                    ui::tilo_actions::ActionKind::Outside,
                    "reading a signed-in page takes the authority to operate it"
                );
                assert_eq!(
                    action.contract.execution,
                    ui::tilo_actions::ExecutionMode::Background { cancellable: false }
                );
            }
            let (_, blank, doc, _) = page_on("about:blank");
            assert!(
                !page_action(&blank, &doc, "read-page")
                    .contract
                    .availability
                    .is_available()
            );
        }

        // Phase `read_returns_the_engine_outline_in_the_declared_shape`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            let invocation = start(
                &mut widget,
                &mut doc,
                &root,
                "read-page",
                &serde_json::json!({}),
            )
            .unwrap();
            assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
            widget.sync(&doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                widget.poll_invocation(invocation)
            else {
                panic!("an answered read must succeed its invocation");
            };
            let read = page_action(&widget, &doc, "read-page");
            let value = output.value.unwrap();
            assert_eq!(value["url"], "https://shop.test/");
            assert_eq!(
                value["outline"],
                "- document \"https://shop.test/ — test page\""
            );
        }

        // Phase `reader_view_toggles_through_the_page_and_ends_with_its_document`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://news.test/story");
            let toggle = |widget: &mut BrowserWidget, doc: &mut AutoCommit| {
                let invocation =
                    start(widget, doc, &root, "reader-view", &serde_json::json!({})).unwrap();
                widget.sync(doc, &root);
                assert!(matches!(
                    widget.poll_invocation(invocation),
                    Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
                ));
            };
            toggle(&mut widget, &mut doc);
            assert_eq!(
                page_requests(&engine),
                vec![PageRequest::Reader { enabled: true }]
            );
            assert!(widget.reader_view);
            assert_eq!(
                page_action(&widget, &doc, "reader-view").label,
                "Close reader view"
            );
            toggle(&mut widget, &mut doc);
            assert_eq!(
                page_requests(&engine).last(),
                Some(&PageRequest::Reader { enabled: false })
            );
            assert!(!widget.reader_view);
            toggle(&mut widget, &mut doc);
            assert!(widget.reader_view);
            widget
                .perform_browser_operation(BrowserOperation::Navigate {
                    url: "https://news.test/next".into(),
                })
                .unwrap();
            assert!(!widget.reader_view, "the next page opens as itself");
            let (_, blank, doc, _) = page_on("about:blank");
            assert!(
                !page_action(&blank, &doc, "reader-view")
                    .contract
                    .availability
                    .is_available()
            );
        }

        // Phase `a_read_on_a_loading_page_waits_for_it_to_settle`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            widget.set_loading_progress(0.4);
            let invocation = start(
                &mut widget,
                &mut doc,
                &root,
                "read-page",
                &serde_json::json!({}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            assert!(
                page_requests(&engine).is_empty(),
                "read before the load settled"
            );
            assert!(widget.poll_invocation(invocation).is_none());
            engine.send(WebViewCommand::Reload);
            widget.sync(&doc, &root);
            assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
            widget.sync(&doc, &root);
            assert!(matches!(
                widget.poll_invocation(invocation),
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // Phase `typed_arguments_reach_the_engine_and_bad_refs_never_do`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            let fill = start(
                &mut widget,
                &mut doc,
                &root,
                "fill-page",
                &serde_json::json!({"element": "d3e7", "text": ""}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            assert!(widget.poll_invocation(fill).is_some());
            let select = start(
                &mut widget,
                &mut doc,
                &root,
                "select-page",
                &serde_json::json!({"element": "d3e8", "values": ["NL", "Belgium"]}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            assert!(widget.poll_invocation(select).is_some());
            let press = start(
                &mut widget,
                &mut doc,
                &root,
                "press-page",
                &serde_json::json!({"key": "Shift+Tab"}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            assert!(widget.poll_invocation(press).is_some());
            assert_eq!(
                page_requests(&engine),
                vec![
                    PageRequest::Fill {
                        element: "d3e7".into(),
                        text: String::new(),
                        secret: false,
                    },
                    PageRequest::Select {
                        element: "d3e8".into(),
                        values: vec!["NL".into(), "Belgium".into()],
                    },
                    PageRequest::Press {
                        element: None,
                        key: PageKey::parse("Shift+Tab").unwrap(),
                    },
                ]
            );
            for (id, args) in [
                ("click-page", serde_json::json!({"element": "#checkout"})),
                ("submit-page", serde_json::json!({"element": ""})),
                ("press-page", serde_json::json!({"key": "Hyper+x"})),
            ] {
                let refused = start(&mut widget, &mut doc, &root, id, &args).unwrap_err();
                assert!(!refused.is_empty(), "{id}");
            }
            assert_eq!(
                page_requests(&engine).len(),
                3,
                "refused requests reached the page"
            );
        }

        // Phase `agent_reads_and_consented_writes_reach_the_engine_typed`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            let secret = page_action(&widget, &doc, "fill-page-secret");
            assert!(
                secret
                    .params()
                    .iter()
                    .any(|param| param.name == "text"
                        && param.ty == ui::tilo_actions::ParamType::Secret),
                "a password is a secret parameter, kept out of logs and history"
            );
            let mut answers = Vec::new();
            for (id, args) in [
                ("read-page-changes", serde_json::json!({})),
                (
                    "read-page-element-at",
                    serde_json::json!({"x": 12.5, "y": 40}),
                ),
                ("wait-page-settled", serde_json::json!({})),
                (
                    "wait-page-settled",
                    serde_json::json!({"quiet_ms": 200, "timeout_ms": 3000}),
                ),
                ("read-page-console", serde_json::json!({"since": 4})),
                ("read-page-network", serde_json::json!({})),
                (
                    "fill-page-secret",
                    serde_json::json!({"element": "d1e3", "text": "hunter2"}),
                ),
                ("submit-page", serde_json::json!({"element": "d1e9"})),
                (
                    "submit-page",
                    serde_json::json!({"element": "d1e9", "cross_origin": true}),
                ),
            ] {
                let invocation = start(&mut widget, &mut doc, &root, id, &args).unwrap();
                widget.sync(&doc, &root);
                let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                    widget.poll_invocation(invocation)
                else {
                    panic!("{id} must succeed its invocation");
                };
                answers.push(output.value.unwrap());
            }
            assert_eq!(
                page_requests(&engine),
                vec![
                    PageRequest::Changes,
                    PageRequest::At { x: 12.5, y: 40.0 },
                    PageRequest::Settle {
                        quiet_ms: 500,
                        timeout_ms: 10_000,
                    },
                    PageRequest::Settle {
                        quiet_ms: 200,
                        timeout_ms: 3000,
                    },
                    PageRequest::Console { since: 4 },
                    PageRequest::Network { since: 0 },
                    PageRequest::Fill {
                        element: "d1e3".into(),
                        text: "hunter2".into(),
                        secret: true,
                    },
                    PageRequest::Submit {
                        element: "d1e9".into(),
                        cross_origin: false,
                    },
                    PageRequest::Submit {
                        element: "d1e9".into(),
                        cross_origin: true,
                    },
                ],
                "a form leaves for another site only when the caller says so"
            );
            for value in &answers {
                assert_eq!(value["provenance"], "web-content", "{value}");
            }
            assert!(
                answers[0]["outline"].is_string(),
                "changes answer outline lines"
            );
            assert_eq!(answers[1]["data"]["x"], 12.5);
            assert_eq!(answers[2]["data"]["settled"], true);
            assert_eq!(answers[4]["data"]["entries"], serde_json::json!([]));
            assert_eq!(answers[5]["data"]["next"], 0);
        }

        // Phase `an_action_that_navigates_completes_on_the_page_it_opened`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/cart");
            engine.push_page_reply(Ok(PageReply {
                url: "https://shop.test/cart".into(),
                message: "Clicked button \"Checkout\"".into(),
                outline: None,
                data: None,
                navigated: true,
                loading: true,
            }));
            let click = start(
                &mut widget,
                &mut doc,
                &root,
                "click-page",
                &serde_json::json!({"element": "d1e4"}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            assert!(
                widget.poll_invocation(click).is_none(),
                "a click that started a navigation waits for it"
            );
            assert!(
                start(
                    &mut widget,
                    &mut doc,
                    &root,
                    "read-page",
                    &serde_json::json!({})
                )
                .is_err(),
                "a second page action is refused while one runs"
            );
            engine.send(WebViewCommand::Navigate(
                "https://shop.test/checkout".into(),
            ));
            widget.sync(&doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                widget.poll_invocation(click)
            else {
                panic!("the settled navigation must complete the click");
            };
            let value = output.value.unwrap();
            assert_eq!(value["url"], "https://shop.test/checkout");
            assert_eq!(value["message"], "Clicked button \"Checkout\"");
            assert_eq!(value["navigated"], true);
        }

        // Phase `engine_refusals_become_typed_invocation_failures`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            engine.push_page_reply(Err(PageFailure::new(
                "stale_element",
                "d1e4 belongs to a page this tab has left; read the page again",
            )));
            let click = start(
                &mut widget,
                &mut doc,
                &root,
                "click-page",
                &serde_json::json!({"element": "d1e4"}),
            )
            .unwrap();
            widget.sync(&doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
                widget.poll_invocation(click)
            else {
                panic!("a refused click must fail its invocation");
            };
            assert_eq!(error.code, "browser.page.stale_element");
            assert!(!error.retryable);
            assert!(
                start(
                    &mut widget,
                    &mut doc,
                    &root,
                    "read-page",
                    &serde_json::json!({})
                )
                .is_ok()
            );
        }
    }

    pub(super) fn layout_at(widget: &mut BrowserWidget, width: f32, height: f32) {
        layout_with_theme(widget, width, height, &Theme::dark());
    }

    fn layout_with_theme(widget: &mut BrowserWidget, width: f32, height: f32, theme: &Theme) {
        layout_keeping_tree(widget, width, height, theme);
    }

    #[test]
    fn a_load_never_moves_the_page() {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine, true);
        layout_at(&mut widget, 1000.0, 700.0);
        let idle = widget.find_rect("browser.surface").unwrap();
        widget
            .perform_browser_operation(BrowserOperation::Navigate {
                url: "https://a.test".into(),
            })
            .unwrap();
        assert!(widget.loading_face().is_some(), "the load is shown");
        let _ = widget.take_structure_dirty();
        layout_at(&mut widget, 1000.0, 700.0);
        let loading = widget.find_rect("browser.surface").unwrap();
        assert_eq!(
            idle, loading,
            "the loading line sits on the toolbar's edge; the page stays put"
        );
    }

    /// Lay the widget out and hand back the tree and the widget's node, for
    /// tests that dispatch events against it.
    pub(super) fn layout_keeping_tree(
        widget: &mut BrowserWidget,
        width: f32,
        height: f32,
        theme: &Theme,
    ) -> (TaffyTree<()>, NodeId) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&state, &root);
        let child = widget.build_layout(&mut tree, theme, &mut nodes);
        widget.sync(&state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size::from_lengths(width, height),
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
        (tree, child)
    }

    /// Press the left button at `point`, as a person would.
    fn click_at(
        widget: &mut BrowserWidget,
        tree: &TaffyTree<()>,
        node: NodeId,
        point: Point,
    ) -> bool {
        let mut state = AutoCommit::new();
        let mut at = ui::testing::Dispatch::new()
            .doc(&mut state)
            .layout(tree, node);
        let mut release_handled = false;
        for event in [
            ui::synthetic_input::cursor_moved(point.x, point.y),
            ui::synthetic_input::mouse_press(),
            ui::synthetic_input::mouse_release(),
        ] {
            release_handled = at.send(widget, &event).handled;
        }
        release_handled
    }

    /// Panels open beside the control that opened them, and a dismissed
    /// question is a declined one.
    #[test]
    fn panels_open_below_their_control_and_dismissing_a_question_declines_it() {
        // Phase `the_site_panel_opens_below_the_toolbar_at_the_site_button`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_set_viewport_width(1000);
            layout_at(&mut widget, 1000.0, 700.0);
            widget.site.open = true;
            layout_at(&mut widget, 1000.0, 700.0);
            layout_at(&mut widget, 1000.0, 700.0);
            let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
            let toolbar = widget.find_rect("browser.toolbar").unwrap();
            let trigger = widget.find_rect("browser.site.info").unwrap();
            assert!(
                panel.y0 >= toolbar.y1,
                "the panel {panel:?} leaves the toolbar {toolbar:?} uncovered"
            );
            assert!((panel.x0 - trigger.x0).abs() < 1.0, "{panel:?} {trigger:?}");
            assert!(panel.y1 <= 700.0);
        }

        // Phase `an_autofill_panel_opens_at_its_settings_row`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_autofill(false);
            layout_at(&mut widget, 1000.0, 800.0);
            layout_at(&mut widget, 1000.0, 800.0);
            layout_at(&mut widget, 1000.0, 800.0);
            assert!(widget.credentials.open);
            let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
            let row = widget.find_rect("browser.credentials.open").unwrap();
            assert!(
                (panel.x1 - row.x1).abs() < 1.0,
                "the panel aligns with its trailing management control: {panel:?} {row:?}"
            );
            assert!(
                panel.y1 <= row.y0 || panel.y0 >= row.y1,
                "the panel {panel:?} must not cover its row {row:?}"
            );
        }

        // Phase `dismissing_a_permission_question_declines_it`.
        {
            fn within(widget: &dyn Widget, id: &str) -> bool {
                ui::widget::find_test_id(widget, id).is_some()
            }
            let capture = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(capture.clone(), true);
            widget.debug_request_permission();
            layout_at(&mut widget, 1000.0, 700.0);
            let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &Theme::dark());
            let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
            assert!(panel.width() > 0.0);
            // The question names its two answers, inside the popover.
            let popover = widget.anchored_panel.as_ref().unwrap();
            assert!(within(popover.as_ref(), "browser.decision.true.1.true"));
            assert!(within(popover.as_ref(), "browser.decision.true.1.false"));
            click_at(&mut widget, &tree, node, Point::new(900.0, 650.0));
            layout_at(&mut widget, 1000.0, 700.0);
            assert!(
                capture
                    .commands()
                    .contains(&WebViewCommand::PermissionDecision {
                        id: 1,
                        allow: false
                    })
            );
        }
    }

    /// The library lists show progress as a bar and search where it is a scope.
    #[test]
    fn library_downloads_show_a_progress_bar_and_search_is_offered_only_where_it_scopes() {
        fn count(widget: &dyn Widget, matches: &dyn Fn(&dyn Widget) -> bool) -> usize {
            usize::from(matches(widget))
                + widget
                    .children()
                    .iter()
                    .map(|child| count(*child, matches))
                    .sum::<usize>()
        }
        let bars = |widget: &BrowserWidget| count(widget, &|node| node.name() == "ProgressBar");
        let searches = |widget: &BrowserWidget| {
            count(widget, &|node| {
                node.test_id()
                    .is_some_and(|id| id.starts_with("search-open"))
            })
        };

        let mut downloads = BrowserWidget::new_headless();
        downloads.debug_show_library("downloads");
        downloads.debug_show_downloads("progress");
        layout_at(&mut downloads, 1280.0, 800.0);
        assert_eq!(bars(&downloads), 1, "one download, one bar");
        assert_eq!(searches(&downloads), 1);
        let entry = downloads.downloads.entries().first().unwrap().clone();
        assert!(
            !download_status_text(&entry).contains('%'),
            "the bar carries the percentage"
        );

        let mut extensions = BrowserWidget::new_headless();
        extensions.debug_show_library("extensions");
        layout_at(&mut extensions, 1280.0, 800.0);
        assert_eq!(
            searches(&extensions),
            0,
            "extensions are not a search scope"
        );
    }

    /// Engine navigation, gestures and canonical operations update the address bar and history buttons.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn engine_navigation_drives_address_bar_and_history_controls() {
        // Phase `nav_gesture_drives_engine_history`.
        {
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
                w.on_nav_gesture(NavDirection::Back, &state, &root),
                "a back gesture is consumed by the embedded WebView",
            );
            assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
            w.on_nav_gesture(NavDirection::Forward, &state, &root);
            assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
        }

        // Phase `bootstrap_seeds_address_bar`.
        {
            let engine = TestEngine::new();
            // Headless so the test never spawns the network filter loader.
            let mut w = BrowserWidget::with_engine(engine, true);
            let (state, root) = synced(&mut w);
            assert_eq!(w.address.with(Clone::clone), DEFAULT_URL);
        }

        // Phase `engine_uri_updates_address_bar_text`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut state, root) = synced(&mut w);

            engine.send(WebViewCommand::Navigate("https://example.test".into()));
            w.sync(&state, &root);

            assert_eq!(w.address.with(Clone::clone), "https://example.test");
            assert_eq!(w.app_title(), "https://example.test — test page");
        }

        // Phase `history_events_drive_button_enablement`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut state, root) = synced(&mut w);

            engine.send(WebViewCommand::Navigate("https://a.test".into()));
            engine.send(WebViewCommand::Navigate("https://b.test".into()));
            w.sync(&state, &root);

            assert!(w.can_go_back);
            assert!(!w.can_go_forward);
        }

        // Phase `canonical_navigation_operations_translate_to_engine_commands`.
        {
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
    }

    /// The person's browsing history, as a capture's profile keeps it.
    /// A browser document bookmarking each `(url, title)`, as items
    /// `b0`, `b1`, ….
    fn bookmarked(pages: &[(&str, &str)]) -> crate::resource::Browser {
        let mut browser = crate::resource::Browser::default();
        for (at, (url, title)) in pages.iter().enumerate() {
            browser.bookmarks.push(
                ui::tilo_resource::Id::from_raw(format!("b{at}")),
                crate::library::Bookmark {
                    url: (*url).into(),
                    title: (*title).into(),
                },
            );
        }
        browser
    }

    fn person_history() -> (
        ui::framework::testing::TestApp<crate::BrowserShell>,
        ui::framework::PersonRecord<crate::history::BrowsingHistory>,
    ) {
        let mut app =
            ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
        let record = app
            .person::<crate::history::BrowsingHistory>()
            .expect("a test profile keeps person records");
        (app, record)
    }

    #[test]
    fn visits_are_the_persons_history_and_workspace_exit_clears_it() {
        let (_app, record) = person_history();
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        w.attach_history(record.clone());
        let (mut doc, root) = synced(&mut w);
        engine.send(WebViewCommand::Navigate("https://visited.test/".into()));
        w.sync(&doc, &root);
        assert!(
            record.with(|history| {
                history
                    .visits
                    .keys()
                    .any(|url| url.starts_with("https://visited.test"))
            }),
            "a finished page is a visit in the person's history"
        );
        assert!(
            !w.history.is_empty(),
            "the window shows the history it records"
        );

        let mut paused = BrowserWidget::with_engine(engine.clone(), true);
        paused.attach_history(record.clone());
        paused.history_paused = w.history_paused.clone();
        w.history_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let before = record.with(|history| history.visits.len());
        engine.send(WebViewCommand::Navigate("https://paused.test/".into()));
        paused.sync(&doc, &root);
        assert_eq!(record.with(|history| history.visits.len()), before);

        w.settings.clear_on_exit = true;
        drop(w);
        assert!(
            !record.with(|history| history.visits.is_empty()),
            "closing one page keeps the history"
        );
        let mut last = BrowserWidget::with_engine(TestEngine::new(), true);
        last.attach_history(record.clone());
        last.settings.clear_on_exit = true;
        drop(crate::BrowserWindow::new(last, false));
        assert!(
            record.with(|history| history.visits.is_empty()),
            "leaving the workspace clears the history"
        );
    }

    #[test]
    fn typing_in_the_address_bar_suggests_and_completes_visited_pages() {
        let (_app, record) = person_history();
        for (url, title, at) in [
            ("https://www.youtube.com/watch?v=1", "Cats - YouTube", 10),
            (
                "https://blog.example.org/why-you-should",
                "Why you should",
                11,
            ),
        ] {
            record
                .invoke(&crate::history::BrowsingHistory::VISIT.with(
                    crate::history::browsing_history::Visit {
                        url: url.into(),
                        title: title.into(),
                        at,
                    },
                ))
                .unwrap();
        }
        // A later window of the same person sees the same visits.
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        w.attach_history(record);
        let (doc, root) = synced(&mut w);
        let mut tree = TaffyTree::new();
        w.build_layout(
            &mut tree,
            &Theme::dark(),
            &mut std::collections::HashMap::default(),
        );
        assert!(w.focus_address_input());
        w.address.set("you".into());
        w.sync(&doc, &root);
        assert!(w.suggestions.is_visible(), "typing shows suggestions");
        let rows = w.suggestions.items().to_vec();
        assert_eq!(
            rows[0].kind,
            omnibox::RowKind::Go,
            "Enter opens the bar's text"
        );
        assert_eq!(rows[0].url, "https://youtube.com");
        assert!(
            rows.iter()
                .any(|row| row.kind == omnibox::RowKind::History && row.title == "Cats - YouTube"),
            "visited pages show with their titles: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.kind == omnibox::RowKind::Search),
            "searching the typed words is always offered"
        );
        assert_eq!(
            w.address.with(Clone::clone),
            "youtube.com",
            "the host is completed inline"
        );
        // The echo of the completion is not new typing.
        w.sync(&doc, &root);
        assert_eq!(w.address.with(Clone::clone), "youtube.com");
        assert_eq!(w.suggestions.items().len(), rows.len());
    }

    /// Address input focus, selection and draft preservation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn address_focus_selects_tracks_navigation_and_preserves_drafts() {
        // Phase `focused_address_tracks_committed_navigation_but_preserves_new_drafts`.
        {
            let setup = || {
                let engine = TestEngine::new();
                for url in ["https://a.test", "https://b.test"] {
                    engine.send(WebViewCommand::Navigate(url.into()));
                }
                let mut widget = BrowserWidget::with_engine(engine.clone(), true);
                layout_at(&mut widget, 900.0, 700.0);
                widget.apply_engine_events();
                assert!(widget.focus_address_input());
                (engine, widget)
            };
            let (_, mut widget) = setup();
            widget.address.set("unsent draft".into());
            widget
                .perform_browser_operation(BrowserOperation::Back)
                .unwrap();
            widget.apply_engine_events();
            assert_eq!(widget.address.with(Clone::clone), "https://a.test");
            widget
                .perform_browser_operation(BrowserOperation::Forward)
                .unwrap();
            widget.apply_engine_events();
            assert_eq!(widget.address.with(Clone::clone), "https://b.test");

            let (_, mut widget) = setup();
            widget
                .perform_browser_operation(BrowserOperation::Back)
                .unwrap();
            widget.address.set("new draft after navigation".into());
            widget.apply_engine_events();
            assert_eq!(
                widget.address.with(Clone::clone),
                "new draft after navigation"
            );

            let (engine, mut widget) = setup();
            widget.address.set("unsent draft".into());
            engine.send(WebViewCommand::Navigate("https://redirect.test".into()));
            widget.apply_engine_events();
            assert_eq!(widget.address.with(Clone::clone), "unsent draft");

            let (engine, mut widget) = setup();
            widget
                .perform_browser_operation(BrowserOperation::Navigate {
                    url: "https://start.test".into(),
                })
                .unwrap();
            engine.send(WebViewCommand::Navigate("https://redirect.test".into()));
            widget.apply_engine_events();
            assert_eq!(widget.address.with(Clone::clone), "https://redirect.test");
        }

        // Phase `focused_address_history_menu_selection_replaces_older_draft`.
        {
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            let mut widget = BrowserWidget::with_engine(engine, true);
            layout_at(&mut widget, 900.0, 700.0);
            widget.apply_engine_events();
            assert!(widget.focus_address_input());
            widget.address.set("older unsent draft".into());
            widget.request_navigation_history(NavigationDirection::Back);
            widget.apply_engine_events();
            assert_eq!(widget.back_entries.len(), 1);
            widget.choose_navigation_entry(0);
            widget.apply_engine_events();
            assert_eq!(widget.address.with(Clone::clone), "https://a.test");
        }

        // Phase `address_focus_selects_all_and_survives_chrome_rebuild`.
        {
            fn state(widget: &dyn Widget) -> Option<String> {
                if widget.test_id() == Some(URL_INPUT_ID) {
                    assert!(widget.is_focused());
                    return ui::undo::get_state(widget);
                }
                widget.children().iter().find_map(|child| state(*child))
            }
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let _ = synced(&mut w);
            let mut tree = TaffyTree::new();
            w.build_layout(
                &mut tree,
                &Theme::dark(),
                &mut std::collections::HashMap::default(),
            );
            assert!(w.focus_address_input());

            let selected = format!("{}:b:0", DEFAULT_URL.len());
            assert_eq!(state(&w).as_deref(), Some(selected.as_str()));
            let mut tree = TaffyTree::new();
            w.build_layout(
                &mut tree,
                &Theme::dark(),
                &mut std::collections::HashMap::default(),
            );
            assert_eq!(state(&w).as_deref(), Some(selected.as_str()));
        }

        // Phase `address_shortcut_focuses_the_chrome_input_after_composition`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let _ = synced(&mut w);
            let mut tree = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

            assert!(w.focus_address_input());
            assert!(w.url_input_focused());
        }
    }

    /// Library rows stay readable across widths and text sizes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn library_rows_stay_readable_at_every_width() {
        // Matching and bounds retain the same semantics as the local view action.
        assert!(library_filter_matches(
            "rust learn",
            &["Rust", "https://rust-lang.org/learn"]
        ));
        assert!(!library_filter_matches(
            "rust missing",
            &["Rust", "https://rust-lang.org/learn"]
        ));
        let bounded =
            bounded_library_filter(&format!("rust {}", "🦀".repeat(MAX_LIBRARY_FILTER_BYTES)));
        assert!(bounded.len() <= MAX_LIBRARY_FILTER_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_library("history");
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "rust learn".into(),
                })
                .unwrap();
            assert_eq!(widget.library_filter.get(), "rust learn");
            assert!(widget.show_library);
            assert!(
                widget
                    .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                        query: "x".repeat(MAX_LIBRARY_FILTER_BYTES + 1)
                    })
                    .is_err()
            );
            layout_at(&mut widget, 390.0, 844.0);
            assert!(
                widget
                    .find_rect(&format!(
                        "{}.open",
                        history_resource_id("https://www.rust-lang.org/learn")
                    ))
                    .is_some()
            );
            assert!(
                widget
                    .find_rect(&format!(
                        "{}.open",
                        history_resource_id("https://example.org/long-form")
                    ))
                    .is_none()
            );
            super::architecture_tests::assert_action_binding(
                &widget,
                "browser.library.clear-filter",
                "browser.library",
                "clear-library-filter",
            );
            widget
                .perform_browser_operation(BrowserOperation::ClearLibraryFilter)
                .unwrap();
            assert!(widget.library_filter.get().is_empty());
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "rust learn".into(),
                })
                .unwrap();
            assert_eq!(
                widget.library_filter.get(),
                "rust learn",
                "the framework filters the production Library view"
            );
        }

        // Phase `compact_history_library_retains_the_hidden_toolbar_privacy_action`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_library("history");
            widget.debug_set_viewport_width(390);
            layout_at(&mut widget, 390.0, 844.0);
            assert!(
                ui::widget::controls::control_action_bindings(&widget)
                    .iter()
                    .any(|binding| {
                        binding.control_id == "browser.library.history-pause"
                            && matches!(
                                &binding.kind,
                                ui::widget::ControlBindingKind::Action(address)
                                    if address.resource == "browser.session"
                                        && address.action == "toggle-pause"
                            )
                    })
            );
        }

        // Phase `populated_library_rows_have_a_bounded_compact_measure_at_all_widths`.
        {
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
            let history_tab = wide.find_rect("browser.library.sections").unwrap();
            let theme = ui::Theme::dark();
            let gutter = f64::from(theme.tokens.spacing.rhythm.group);
            let readable = f64::from(theme.tokens.metrics.readable_width);
            assert!(
                (history_tab.x0 - gutter).abs() < 1.0,
                "desktop title, tabs, and rows share the page's leading edge: {history_tab:?}"
            );
            assert!(
                first.x1 <= gutter + readable + 1.0,
                "desktop row actions stay inside the readable Library measure: {first:?}"
            );
            assert!(first.height() <= 60.0);
            assert!(
                second.y0 - first.y0 < 80.0,
                "two-line metadata rows must not expand into oversized slabs"
            );

            let mut narrow = BrowserWidget::new_headless();
            narrow.debug_show_library("bookmarks");
            narrow.present_state(&bookmarked(&[(
                "https://www.rust-lang.org/learn",
                "Learn Rust",
            )]));
            narrow.debug_set_viewport_width(390);
            layout_at(&mut narrow, 390.0, 844.0);
            let open = narrow
                .find_rect(&format!(
                    "{}.open",
                    bookmark_resource_id(&ui::tilo_resource::Id::from_raw("b0"))
                ))
                .unwrap();
            let remove = narrow
                .find_rect(&format!(
                    "{}.remove",
                    bookmark_resource_id(&ui::tilo_resource::Id::from_raw("b0"))
                ))
                .unwrap();
            assert!(open.width() > 0.0 && remove.width() > 0.0);
            assert!(
                remove.x1 <= 390.0,
                "row actions stay reachable on narrow layouts"
            );
            let sections = narrow.find_rect("browser.library.sections").unwrap();
            assert!(
                sections.x0 >= 0.0 && sections.x1 <= 390.0,
                "every Library destination stays visible instead of overflowing horizontally"
            );
        }

        // Phase `large_text_desktop_library_keeps_a_centered_reading_measure`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_library("history");
            widget.debug_set_viewport_width(1_200);
            layout_with_theme(
                &mut widget,
                1_200.0,
                800.0,
                &Theme::dark().with_text_scale(2.0),
            );

            let history_tab = widget.find_rect("browser.library.sections").unwrap();
            let open = widget
                .find_rect(&format!(
                    "{}.open",
                    history_resource_id("https://www.rust-lang.org/learn")
                ))
                .unwrap();
            let theme = Theme::dark();
            let gutter = f64::from(theme.tokens.spacing.rhythm.group);
            let readable = f64::from(theme.tokens.metrics.readable_width);
            assert!(
                (history_tab.x0 - gutter).abs() < 1.0,
                "large text keeps the Library on the page's leading edge: {history_tab:?}"
            );
            assert!(
                open.x1 <= gutter + readable + 1.0,
                "large-text row actions stay inside the readable Library measure: {open:?}"
            );
        }
    }

    /// Address validation errors and modal suggestions.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn address_validation_and_suggestions_dispatch_only_web_navigation() {
        // Phase `address_validation_never_dispatches_non_web_inputs_and_keeps_a_recoverable_error`.
        {
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

        // Phase `address_validation_accepts_local_web_hosts_but_not_local_files`.
        {
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

        // Phase `stale_engine_completion_cannot_clear_a_rejected_address_error`.
        {
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

        // Phase `address_suggestions_are_modal_and_a11y_actions_queue_canonical_navigation`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.attach_dispatcher(ui::framework::testing::dispatcher("so.tilo.browser"));
            widget.debug_force_suggestions(&[crate::history::HistoryEntry {
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
                &mut Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            let invocation = ui::widget::controls::take_control_invocation(&mut widget)
                .expect("accessibility activation must queue Browser's action binding");
            assert_eq!(invocation.address.resource, "browser.page");
            assert_eq!(invocation.address.action, "navigate");
            assert!(Widget::modal_capture(&widget).is_none());
        }
    }

    /// Settings, toolbar and page viewport at narrow widths.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn narrow_chrome_keeps_settings_toolbar_and_page_viewport_usable() {
        // Phase `address_editor_opens_and_requires_opt_in_through_narrow_settings`.
        {
            let (_app, book) = crate::autofill::tests::address_book();
            let mut widget = BrowserWidget::new_headless();
            widget.attach_address_book(book.clone());
            let (mut doc, root) = synced(&mut widget);
            widget.show_settings = true;
            widget.structure_dirty = true;
            layout_at(&mut widget, 390.0, 700.0);
            for id in [
                "browser.address.open",
                "browser.address.enable",
                "browser.address.new",
                "browser.address.cancel",
                "browser.credentials.close",
            ] {
                let result = widget
                    .start_encoded(
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec(id).unwrap(),
                        &mut doc,
                        &root,
                    )
                    .unwrap();
                assert!(
                    result.is_some(),
                    "address editor control must be reachable: {id}"
                );
                assert!(widget.credentials.sync(&widget.engine));
                widget.structure_dirty = true;
                layout_at(&mut widget, 390.0, 700.0);
                if id == "browser.address.open" {
                    assert!(
                        !crate::autofill::AutofillStore::open(
                            BrowsingMode::Normal,
                            Some(book.clone())
                        )
                        .enabled(),
                        "opening does not opt in"
                    );
                }
                if id == "browser.address.new" {
                    let theme = Theme::dark();
                    let mut text = TextRenderer::new();
                    let mut scene = Scene::new();
                    let presence = PresenceCtx::default();
                    widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                    widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                    if let Some(directory) = std::env::var_os("TILO_CREDENTIAL_RENDER_EVIDENCE") {
                        let directory = std::path::PathBuf::from(directory);
                        std::fs::create_dir_all(&directory).unwrap();
                        ui::offscreen::OffscreenRenderer::new(390, 700).render_png(
                            &scene,
                            theme.tokens.colors.surfaces.content,
                            directory.join("address-editor-narrow.png"),
                        );
                    }
                }
            }
            assert!(!widget.credentials.open);
            let store = crate::autofill::AutofillStore::open(BrowsingMode::Normal, Some(book));
            assert!(store.enabled());
            assert!(
                store.profiles().is_empty(),
                "discarded editor must not save a profile"
            );
        }

        // Phase `credentials_settings_launcher_and_overlay_actions_work_at_narrow_width`.
        {
            let mut widget = BrowserWidget::new_headless();
            let (mut doc, root) = synced(&mut widget);
            widget.show_settings = true;
            widget.structure_dirty = true;
            layout_at(&mut widget, 390.0, 700.0);
            for id in ["browser.credentials.open", "browser.credentials.close"] {
                let result = widget
                    .start_encoded(
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec(id).unwrap(),
                        &mut doc,
                        &root,
                    )
                    .unwrap();
                assert!(
                    result.is_some(),
                    "credential action must reach its visible control"
                );
                assert!(widget.credentials.sync(&widget.engine));
                widget.structure_dirty = true;
                layout_at(&mut widget, 390.0, 700.0);
                if id.ends_with("open") {
                    assert!(widget.credentials.open);
                    let panel = widget.anchored_panel.as_ref().unwrap();
                    assert!(
                        panel.traps_focus(),
                        "modal must enable scoped host Tab navigation"
                    );
                    assert!(panel.get_rect().x1 <= widget.rect.x1);
                    assert!(widget.modal_capture().is_some());
                }
            }
            assert!(!widget.credentials.open);
            assert!(widget.modal_capture().is_none());
        }

        // Phase `narrow_chrome_keeps_a_nonzero_page_viewport`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_set_viewport_width(390);
            let mut state = AutoCommit::new();
            let root = automerge::ROOT;
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            widget.sync(&state, &root);
            let child = widget.build_layout(&mut tree, &theme, &mut nodes);
            widget.sync(&state, &root);
            let layout_root = tree
                .new_with_children(
                    Style {
                        size: Size::from_lengths(390.0, 844.0),
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
                "compact chrome preserves a usable address/search field: {}",
                address.width()
            );
        }

        // Phase `toolbar_surface_controls_advertise_the_action_their_labels_promise`.
        {
            fn address_for(
                widget: &BrowserWidget,
                control: &str,
            ) -> ui::tilo_actions::ActionAddress {
                ui::widget::controls::control_action_bindings(widget)
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
            let doc = AutoCommit::new();
            widget.sync(&doc, &automerge::ROOT);
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            assert_eq!(
                address_for(&widget, "browser.library").action,
                "show-history"
            );
            super::architecture_tests::activate_overflow_command(&mut widget, "Settings");
            assert!(widget.run_pending_commands());
            assert!(
                widget
                    .pending_controls
                    .iter()
                    .any(
                        |invocation| invocation.address.resource == "browser.settings"
                            && invocation.address.action == "show-settings"
                    ),
                "the actual Settings menu command queues its canonical action"
            );

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
            assert_eq!(address_for(&widget, "settings.close").action, "show-page");
        }

        // Phase `loading_progress_keeps_toolbar_above_the_page_surface`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_show_loading();
            let mut state = AutoCommit::new();
            let root = automerge::ROOT;
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            widget.sync(&state, &root);
            let child = widget.build_layout(&mut tree, &theme, &mut nodes);
            widget.sync(&state, &root);
            let layout_root = tree
                .new_with_children(
                    Style {
                        size: Size::from_lengths(900.0, 620.0),
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
            let toolbar = widget.find_rect("browser.stop").unwrap();
            let viewport = widget.find_rect("browser.surface").unwrap();
            assert!(toolbar.height() > 0.0);
            assert!(
                viewport.y0 > toolbar.y1,
                "loading must reserve a strip below the toolbar, not let WebView cover it"
            );
        }
    }

    /// Site panel retention, composition, narrow rendering and resource actions.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn site_panel_stays_retained_composed_and_actionable() {
        // Phase `site_panel_remains_in_workspace_tree_across_sync`.
        {
            let mut widget = BrowserWidget::new_headless();
            layout_at(&mut widget, 800.0, 588.0);
            widget.site.open = true;
            let window = crate::BrowserWindow::new(widget, false);
            let mut harness = ui::headless::FrameHarness::new(
                ui::state::State::new(),
                Box::new(window),
                (800, 632),
            );
            for _ in 0..3 {
                harness.relayout();
            }
            assert!(harness.targets().contains_key("browser.site.pages"));
            assert!(harness.targets().contains_key("browser.site.close"));
        }

        // Phase `site_panel_composes_above_page_across_repeated_layouts`.
        {
            let mut widget = BrowserWidget::new_headless();
            layout_at(&mut widget, 800.0, 560.0);
            widget.site.open = true;
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let presence = PresenceCtx::default();
            for pass in 0..3 {
                layout_at(&mut widget, 800.0, 560.0);
                let panel = widget.anchored_panel.as_ref().unwrap();
                assert!(panel.get_rect().height() > 100.0);
                let mut scene = Scene::new();
                widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                let page_tags = scene.encoding().draw_tags.len();
                widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                assert!(
                    scene.encoding().draw_tags.len() > page_tags + 5,
                    "overlay must emit visible content"
                );
                if let Some(directory) = std::env::var_os("TILO_SITE_RENDER_EVIDENCE") {
                    let path = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&path).unwrap();
                    let mut renderer = ui::offscreen::OffscreenRenderer::new(800, 560);
                    renderer.render_png(
                        &scene,
                        theme.tokens.colors.surfaces.content,
                        path.join(format!("panel-pass-{pass}.png")),
                    );
                }
            }
        }

        // Phase `populated_site_panels_render_at_narrow_width`.
        {
            let mut widget = BrowserWidget::new_headless();
            let (mut doc, root) = synced(&mut widget);
            widget.site.navigation("https://example.com");
            widget.site.open = true;
            widget.site.sync(&widget.engine);
            assert!(
                widget
                    .site
                    .apply(WebViewEvent::SiteStatus(ui::webview::SiteStatus {
                        request_id: 1,
                        origin: "https://example.com".into(),
                        generation: 2,
                        blocked: 4,
                        global_enabled: true,
                        excepted: false,
                        effective: true,
                        security: "secure".into(),
                        certificate_status: 0,
                        tls_version: 6,
                        content_status: 0,
                        subject: "example.com".into(),
                        issuer: "Cloudflare TLS Issuing ECC CA 3".into(),
                        expires: 13_437_613_041_000_000.0,
                        permissions: ["camera", "microphone", "location", "notifications"]
                            .into_iter()
                            .map(|name| (name.into(), "ask".into()))
                            .collect(),
                    }))
            );
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let presence = PresenceCtx::default();
            for information in [false, true] {
                layout_at(&mut widget, 390.0, 700.0);
                // Match the retained app layout after its first parent size is known.
                layout_at(&mut widget, 390.0, 700.0);
                if information {
                    assert!(
                        widget
                            .start_encoded(
                                OperationKind::View,
                                "tilo.ui.segmented.select",
                                &serde_json::to_vec(
                                    &serde_json::json!({"control":"browser.site.pages", "index":1})
                                )
                                .unwrap(),
                                &mut doc,
                                &root
                            )
                            .unwrap()
                            .is_some()
                    );
                    widget.site.sync(&widget.engine);
                    layout_at(&mut widget, 390.0, 700.0);
                }
                let rect = widget.anchored_panel.as_ref().unwrap().get_rect();
                assert!(
                    rect.x0 >= 0.0 && rect.x1 <= 390.0 && rect.height() > 100.0,
                    "{rect:?}"
                );
                let mut scene = Scene::new();
                widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                if let Some(directory) = std::env::var_os("TILO_SITE_RENDER_EVIDENCE") {
                    let path = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&path).unwrap();
                    let mut renderer = ui::offscreen::OffscreenRenderer::new(390, 700);
                    let name = if information {
                        "narrow-site-info.png"
                    } else {
                        "narrow-protection.png"
                    };
                    renderer.render_png(
                        &scene,
                        theme.tokens.colors.surfaces.content,
                        path.join(name),
                    );
                }
            }
        }

        // Phase `site_panel_resource_actions_reach_the_retained_panel`.
        {
            let mut widget = BrowserWidget::new_headless();
            let (mut doc, root) = synced(&mut widget);
            layout_at(&mut widget, 1000.0, 700.0);
            widget.site.open = true;
            layout_at(&mut widget, 1000.0, 700.0);
            assert!(widget.anchored_panel.is_some());
            let result = widget
                .start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.site.close").unwrap(),
                    &mut doc,
                    &root,
                )
                .unwrap();
            assert!(
                result.is_some(),
                "native button operation must reach overlay child"
            );
            widget.site.sync(&widget.engine);
            assert!(!widget.site.open);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the real panel action and ordered asynchronous replies in one regression fixture."
    )]
    fn site_protection_reload_updates_chrome_only_after_correlated_success() {
        // Keep engine replies under test control: TestEngine records commands,
        // while this queue models the native helper's asynchronous event order.
        struct QueuedEngine {
            events: std::sync::Mutex<Vec<WebViewEvent>>,
            capture: Arc<TestEngine>,
        }
        impl WebViewEngine for QueuedEngine {
            fn send(&self, command: WebViewCommand) {
                self.capture.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.capture.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.events.lock().unwrap())
            }
        }

        const ORIGIN: &str = "https://protection.test";
        const URL: &str = "https://protection.test/page";
        const GENERATION: u64 = 3;
        for reload_succeeds in [false, true] {
            let capture = TestEngine::new();
            let engine = Arc::new(QueuedEngine {
                events: std::sync::Mutex::new(Vec::new()),
                capture: capture.clone(),
            });
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            let (mut doc, root) = synced(&mut widget);
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::UriChanged(URL.into()));
            widget.apply_engine_events();
            widget.set_loading_progress(0.2);
            widget
                .perform_browser_operation(BrowserOperation::Stop)
                .unwrap();
            widget
                .daily
                .act(crate::daily_controls::Action::Open, &widget.engine);
            widget.daily.buffer_text("existing match");
            widget.daily.sync(&widget.engine);
            let find_id = capture
                .commands()
                .into_iter()
                .find_map(|command| match command {
                    WebViewCommand::Find {
                        id, next: false, ..
                    } => Some(id),
                    _ => None,
                })
                .expect("Find has an active native request");
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::FindResult {
                    id: find_id,
                    count: 4,
                    active: 2,
                    final_update: true,
                });
            widget.apply_engine_events();

            widget.site.open = true;
            widget.sync(&doc, &root);
            let status_request = capture
                .commands()
                .into_iter()
                .rev()
                .find_map(|command| match command {
                    WebViewCommand::RequestSiteStatus { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .expect("open panel requests native site status");
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::SiteStatus(ui::webview::SiteStatus {
                    request_id: status_request,
                    origin: ORIGIN.into(),
                    generation: GENERATION,
                    blocked: 4,
                    global_enabled: true,
                    excepted: false,
                    effective: true,
                    security: "secure".into(),
                    certificate_status: 0,
                    tls_version: 6,
                    content_status: 0,
                    subject: String::new(),
                    issuer: String::new(),
                    expires: 0.0,
                    permissions: Vec::new(),
                }));
            widget.apply_engine_events();
            layout_at(&mut widget, 1000.0, 700.0);
            assert!(widget.anchored_panel.is_some());
            assert!(
                widget
                    .start_encoded(
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.site.toggle").unwrap(),
                        &mut doc,
                        &root
                    )
                    .unwrap()
                    .is_some(),
                "the real protection button must accept the action"
            );
            widget.sync(&doc, &root);
            let toggle_request = capture
                .commands()
                .into_iter()
                .rev()
                .find_map(|command| match command {
                    WebViewCommand::SiteAction {
                        request_id,
                        origin,
                        generation,
                        action,
                    } if action == "disable" => {
                        assert_eq!(origin, ORIGIN);
                        assert_eq!(generation, GENERATION);
                        Some(request_id)
                    }
                    _ => None,
                })
                .expect("the panel sends an origin-scoped protection change");
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::SiteResult {
                    request_id: toggle_request,
                    origin: ORIGIN.into(),
                    generation: GENERATION,
                    action: "disable".into(),
                    success: true,
                    message: "Changed".into(),
                });
            widget.sync(&doc, &root);
            widget.sync(&doc, &root);
            let reload_request = capture
                .commands()
                .into_iter()
                .rev()
                .find_map(|command| match command {
                    WebViewCommand::SiteAction {
                        request_id,
                        origin,
                        generation,
                        action,
                    } if action == "reload" => {
                        assert_eq!(origin, ORIGIN);
                        assert_eq!(generation, GENERATION);
                        Some(request_id)
                    }
                    _ => None,
                })
                .expect("the accepted protection change submits a scoped reload");

            let reply = |request_id, success| WebViewEvent::SiteResult {
                request_id,
                origin: ORIGIN.into(),
                generation: GENERATION,
                action: "reload".into(),
                success,
                message: "Reload result".into(),
            };
            let assert_stopped = |widget: &BrowserWidget| {
                assert!(
                    widget.loading_stopped,
                    "an unaccepted reload must not resume loading chrome"
                );
                assert!((widget.progress - 1.0).abs() < f64::EPSILON);
                assert!(widget.daily.open, "an unaccepted reload must preserve Find");
            };
            assert_stopped(&widget);
            engine
                .events
                .lock()
                .unwrap()
                .push(reply(reload_request + 1, true));
            widget.apply_engine_events();
            assert_stopped(&widget);
            assert!(!capture.commands().contains(&WebViewCommand::StopFinding));

            engine
                .events
                .lock()
                .unwrap()
                .push(reply(reload_request, reload_succeeds));
            widget.apply_engine_events();
            if reload_succeeds {
                assert!(!widget.loading_stopped);
                assert!(widget.progress.abs() < f64::EPSILON);
                assert!(!widget.daily.open);
                assert!(capture.commands().contains(&WebViewCommand::StopFinding));
                engine.events.lock().unwrap().extend([
                    WebViewEvent::UriChanged(URL.into()),
                    WebViewEvent::LoadProgress(0.4),
                ]);
                widget.apply_engine_events();
                assert!(
                    (widget.progress - 0.4).abs() < f64::EPSILON,
                    "same-URL reload progress must resume after Stop"
                );
                engine
                    .events
                    .lock()
                    .unwrap()
                    .push(reply(reload_request, true));
                widget.apply_engine_events();
                assert!(
                    (widget.progress - 0.4).abs() < f64::EPSILON,
                    "a repeated reply must not restart progress"
                );
                assert!(!widget.daily.open);
            } else {
                engine
                    .events
                    .lock()
                    .unwrap()
                    .push(WebViewEvent::LoadProgress(0.4));
                widget.apply_engine_events();
                assert_stopped(&widget);
                assert!(!capture.commands().contains(&WebViewCommand::StopFinding));
            }
            assert!(
                !capture.commands().contains(&WebViewCommand::Reload),
                "chrome reset must not send a second unscoped reload"
            );
        }
    }

    /// Picture-in-picture feedback placement and lifetime.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pip_feedback_is_anchored_clamped_and_settles() {
        // The monotonic clock is pinned, so a feedback note cannot lapse while
        // a loaded machine runs the phase.
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            // Phase `narrow_pip_feedback_stays_inside_the_window_beside_the_overflow_menu`.
            {
                let mut widget = BrowserWidget::new_headless();
                widget.debug_set_viewport_width(390);
                widget.debug_show_empty_pip_feedback();
                layout_at(&mut widget, 390.0, 844.0);
                layout_at(&mut widget, 390.0, 844.0);

                let rect = widget.anchored_panel.as_ref().unwrap().get_rect();
                assert!(rect.width() > 0.0);
                assert!(rect.x0 >= 0.0 && rect.x1 <= 390.0, "{rect:?}");
                let overflow = widget.find_rect(toolbar::OVERFLOW_ID).unwrap();
                assert!(
                    rect.y0 >= overflow.y1,
                    "the note opens below the control that holds the command"
                );

                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                widget.draw_overlay(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &Theme::dark(),
                    &PresenceCtx::empty(),
                );
                assert!(
                    !scene.encoding().draw_tags.is_empty(),
                    "compact feedback must remain visible even without a PiP toolbar button"
                );
            }

            // Phase `pip_feedback_needs_no_animation_frames_and_wakes_at_its_expiry`.
            {
                let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
                w.pip_feedback = Some(PipFeedback {
                    kind: PipFeedbackKind::NoVideo,
                    started: ui::time::now(),
                    persistent: false,
                });
                assert!(!w.needs_animation_frame());
                assert!(w.next_wake_deadline().is_some());
                w.pip_feedback = Some(PipFeedback::persistent(PipFeedbackKind::NoVideo));
                assert!(w.next_wake_deadline().is_none());
            }

            // Phase `actionable_pip_feedback_does_not_expire_under_the_pointer`.
            {
                let mut w = BrowserWidget::new_headless();
                w.pip_feedback = Some(PipFeedback {
                    kind: PipFeedbackKind::NoVideo,
                    started: ui::time::now(),
                    persistent: false,
                });
                layout_at(&mut w, 800.0, 600.0);
                layout_at(&mut w, 800.0, 600.0);
                let rect = w.anchored_panel.as_ref().unwrap().get_rect();
                w.pip_feedback = Some(PipFeedback {
                    kind: PipFeedbackKind::NoVideo,
                    started: ui::time::now()
                        .checked_sub(PIP_FEEDBACK_LIFETIME + std::time::Duration::from_secs(1))
                        .unwrap_or_else(ui::time::Instant::now),
                    persistent: false,
                });
                w.cursor = rect.center();
                w.apply_panel_events();
                assert!(w.pip_feedback.is_some());

                w.cursor = Point::ZERO;
                w.apply_panel_events();
                assert!(w.pip_feedback.is_none());
            }
        });
    }

    /// Picture-in-picture video probing, no-video fallback and host path.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pip_floats_the_video_or_falls_back_to_shell_selection() {
        // The monotonic clock is pinned, so a feedback note cannot lapse while
        // a loaded machine runs the phase.
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            // Phase `pip_button_probes_and_floats_only_the_video_rect`.
            {
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
                w.sync(&state, &root); // accepts/sends the asynchronous probe
                assert!(
                    engine.commands().iter().any(|command| matches!(
                        command,
                        WebViewCommand::RequestMediaElement { .. }
                    ))
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
                    ui::time::now()
                        .checked_sub(std::time::Duration::from_millis(1))
                        .unwrap_or_else(ui::time::Instant::now),
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

            // Phase `pip_without_video_explains_the_shell_selection_fallback`.
            {
                let engine = TestEngine::new();
                let mut w = BrowserWidget::with_engine(engine, true);
                let (mut state, root) = synced(&mut w);

                w.begin_pip_probe();
                w.sync(&state, &root);
                w.sync(&state, &root);

                assert!(w.take_floating_surface_request().is_none());
                assert_eq!(
                    w.pip_feedback.as_ref().map(|feedback| feedback.kind),
                    Some(PipFeedbackKind::NoVideo)
                );
            }

            // Phase `no_video_select_area_requests_the_shared_shell_pip_picker_once`.
            {
                let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
                w.set_pip_feedback(Some(PipFeedback {
                    kind: PipFeedbackKind::NoVideo,
                    started: ui::time::now(),
                    persistent: false,
                }));
                layout_at(&mut w, 1000.0, 700.0);
                let (tree, node) = layout_keeping_tree(&mut w, 1000.0, 700.0, &Theme::dark());
                let choose = w
                    .find_rect("browser.pip.select-area")
                    .expect("shared selection control");
                assert!(
                    click_at(&mut w, &tree, node, choose.center()),
                    "the real selection control handles its complete click"
                );
                let invocation = ui::widget::controls::take_control_invocation(&mut w)
                    .expect("pointer emits its canonical control invocation");
                assert_eq!(
                    invocation.address.resource,
                    "tilo.control:browser.pip.select-area"
                );
                assert_eq!(invocation.address.action, "invoke");
                let (mut state, root) = synced(&mut w);
                assert!(
                    w.start_encoded(
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.pip.select-area").unwrap(),
                        &mut state,
                        &root
                    )
                    .unwrap()
                    .is_some()
                );
                w.sync(&state, &root);
                assert_eq!(
                    w.take_capture_studio_request(),
                    Some(ui::widget::CaptureMode::Pip)
                );
                assert!(w.take_capture_studio_request().is_none());
                assert!(w.take_floating_surface_request().is_none());
                assert!(w.pip_feedback.is_none());
            }

            // Phase `no_video_fallback_floats_and_tracks_the_page_viewport`.
            {
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
                let ui::floating_surface::FloatingSource::AppRegion { region, .. } = request.source
                else {
                    panic!(
                        "page fallback should track the page viewport, not invent a media element"
                    );
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
                    !engine.commands().iter().any(|command| matches!(
                        command,
                        WebViewCommand::RequestMediaElementById { .. }
                    )),
                    "page fallback must not be closed by semantic-video polling"
                );
                w.on_floating_surface_host_message(
                    &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: PIP_ID.into() },
                );
                assert!(!w.active_pip && !w.pip_page_fallback);
            }

            // Phase `active_pip_closes_when_its_semantic_element_disappears`.
            {
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
                    ui::time::now()
                        .checked_sub(std::time::Duration::from_millis(1))
                        .unwrap_or_else(ui::time::Instant::now),
                );

                w.poll_active_pip_element();
                w.apply_engine_events();

                assert!(!w.active_pip);
                assert!(w.active_pip_element.is_none());
                assert!(w.active_floating_surface_ids().is_empty());
                assert!(w.pip_feedback.is_some());
            }

            // Phase `picture_in_picture_host_path_waits_for_engine_success_or_error`.
            {
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
                let resources = [w.published()];
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
                    w.admit(invocation, BrowserOperation::TogglePictureInPicture)
                        .map(Some)
                        .unwrap(),
                    Some(ui::framework::Effected::Pending)
                );
                assert!(w.poll_invocation(invocation).is_none());
                w.sync(&doc, &root);
                let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                    w.poll_invocation(invocation)
                else {
                    panic!("media acceptance must terminally succeed the admitted invocation");
                };

                let mut unavailable = BrowserWidget::with_engine(TestEngine::new(), true);
                let (mut doc, root) = synced(&mut unavailable);
                let invocation = ui::tilo_actions::InvocationId::new();
                assert!(matches!(
                    unavailable
                        .admit(invocation, BrowserOperation::TogglePictureInPicture)
                        .map(Some)
                        .unwrap(),
                    Some(ui::framework::Effected::Pending)
                ));
                unavailable.sync(&doc, &root);
                assert!(matches!(
                    unavailable.poll_invocation(invocation),
                    Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
                ));
            }
        });
    }

    /// Engine selection, resource surface binding and session restore identity.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn engine_surface_and_session_restore_keep_truthful_page_identity() {
        // Phase `headless_engine_has_deterministic_page`.
        {
            let w = BrowserWidget::new_headless();
            let slot = w.engine.frames();
            let frame = slot.lock().unwrap().clone().expect("pre-rendered frame");
            assert!(frame.width > 0 && frame.height > 0);
        }

        // Phase `default_engine_failure_never_becomes_a_simulated_page`.
        {
            let (engine, problem) =
                engine_or_error(Err("Could not start Chromium at /x: denied".into()));
            assert_eq!(problem, Some(EngineProblem::Failed));
            let _clone: SharedEngine = Arc::clone(&engine);
            engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
            assert!(engine.frames().lock().unwrap().is_none());
            // The person reads what happened; the cause stays in the log.
            assert!(engine.drain_events().iter().any(|event| matches!(event,
                WebViewEvent::LoadFailed { error, .. }
                    if error == ui::webview::chromium::runtime::FAILED_MESSAGE)));
        }

        // Phase `the_page_surface_is_named_after_the_page_title`.
        {
            fn surface_name(widget: &dyn Widget) -> Option<String> {
                if widget.test_id() == Some("browser.surface") {
                    return widget.accessibility().and_then(|node| node.name);
                }
                widget
                    .children()
                    .iter()
                    .find_map(|child| surface_name(*child))
            }
            let mut w = BrowserWidget::new_headless();
            layout_at(&mut w, 900.0, 600.0);
            layout_at(&mut w, 900.0, 600.0);
            let composed = w.composed.element().expect("composed browser");
            assert_eq!(
                surface_name(composed).as_deref(),
                Some(format!("{SHOTS_URL} — test page").as_str()),
                "assistive technology reads the loaded page by its title"
            );
        }

        // Phase `a_missing_engine_offers_its_install_instead_of_instructions`.
        {
            assert_eq!(
                ENGINE_INSTALL_URI,
                "tilo-store:runtime/so.tilo.browser/tilo-runtime/2026.10.17-chromium",
                "repair names the consuming app and exact declaration, not a catalogue alias"
            );
            let engine = unavailable_engine(BrowsingMode::Normal, EngineProblem::NotInstalled);
            engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
            let message = engine
                .drain_events()
                .into_iter()
                .find_map(|event| match event {
                    WebViewEvent::LoadFailed { error, .. } => Some(error),
                    _ => None,
                })
                .expect("navigation answers with the problem");
            assert_eq!(
                message,
                ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE
            );
            for developer_word in ["build-host", "TILO_", "python", "Chromium"] {
                assert!(!message.contains(developer_word), "{message}");
            }

            let mut w = BrowserWidget::with_engine(engine, false);
            w.engine_problem = Some(EngineProblem::NotInstalled);
            let _ = synced(&mut w);
            let resources = [w.published()];
            let page = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.page")
                .unwrap();
            let install = page
                .actions
                .iter()
                .find(|action| action.id == "install-engine")
                .expect("the install is an action on the page");
            assert_eq!(install.kind, OperationKind::Effect);
            assert!(install.contract.availability.is_available());
            layout_at(&mut w, 900.0, 600.0);
            assert!(
                ui::widget::controls::control_action_bindings(&w)
                    .iter()
                    .any(|binding| binding.control_id == "browser.install-engine"
                        && matches!(
                            &binding.kind,
                            ui::widget::ControlBindingKind::Action(address)
                                if address.resource == "browser.page"
                                    && address.action == "install-engine"
                        )),
                "the page area's Install button runs the page's install action"
            );

            // Once the engine runs there is nothing to install.
            let mut running = BrowserWidget::with_engine(TestEngine::new(), true);
            let _ = synced(&mut running);
            let resources = [running.published()];
            let page = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.page")
                .unwrap();
            assert!(
                !page
                    .actions
                    .iter()
                    .any(|action| action.id == "install-engine")
            );
            assert!(
                running
                    .perform_browser_operation(BrowserOperation::InstallEngine)
                    .is_err()
            );
        }

        // Phase `resource_surface_binds_typed_navigation_and_reports_history_availability`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut doc, root) = synced(&mut w);
            let resources = [w.published()];
            assert_eq!(resources[0].id.0, crate::resource::Browser::KIND);
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
            assert!(
                page.actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "the page row offers only what this window does"
            );
            let back = page.actions.iter().find(|a| a.id == "back").unwrap();
            assert!(!back.contract.availability.is_available());
            let navigate = page.actions.iter().find(|a| a.id == "navigate").unwrap();
            assert_eq!(navigate.params()[0].name.as_ref(), "url");
            // Another page is a desktop tab: "Open in new tab" asks the
            // desktop and leaves this window on its own page.
            let open_tab = page.actions.iter().find(|a| a.id == "open-tab").unwrap();
            assert!(
                open_tab
                    .params()
                    .iter()
                    .any(|param| param.name.as_ref() == "url")
            );
            assert!(open_tab.contract.validate_declaration().is_empty());
            let mut args = serde_json::Map::new();
            args.insert("url".into(), serde_json::json!("example.test/next"));
            args.insert("background".into(), serde_json::json!(true));
            let operation = effect_operation(&open_tab.build_json(&args).unwrap().payload);
            assert!(matches!(
                &operation,
                BrowserOperation::OpenInNewTab { url, background: true } if url == "example.test/next"
            ));
            let _ = ui::shell_tabs::take_requests();
            let before = engine.commands().len();
            w.perform_browser_operation(operation).unwrap();
            assert_eq!(
                ui::shell_tabs::take_requests(),
                vec![ui::shell_tabs::TabRequest::OpenBeside {
                    target: Some("https://example.test/next".into()),
                    background: true,
                }]
            );
            assert!(
                !engine.commands()[before..]
                    .iter()
                    .any(|command| matches!(command, WebViewCommand::Navigate(_))),
                "this window keeps its page"
            );
            assert_eq!(
                navigate.contract.execution,
                ui::tilo_actions::ExecutionMode::Background { cancellable: false }
            );
            assert!(navigate.contract.validate_declaration().is_empty());
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
                .find(|action| action.id == "set-library-filter")
                .unwrap();
            assert_eq!(filter.kind, OperationKind::View);
            assert_eq!(filter.params()[0].name.as_ref(), "query");
            assert!(filter.contract.validate_declaration().is_empty());
            assert!(
                settings
                    .actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "the preferences are the resource's own document actions"
            );
            let mut args = serde_json::Map::new();
            args.insert("url".into(), serde_json::json!("example.test"));
            let encoded = navigate.build_json(&args).unwrap();
            let invocation = ui::tilo_actions::InvocationId::new();
            assert_eq!(
                w.admit(invocation, effect_operation(&encoded.payload))
                    .map(Some)
                    .unwrap(),
                Some(ui::framework::Effected::Pending)
            );
            assert_eq!(
                engine.current_uri().as_deref(),
                Some("https://example.test")
            );
            w.sync(&doc, &root);
            assert!(matches!(
                w.poll_invocation(invocation),
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
            ));

            let mut filter_args = serde_json::Map::new();
            filter_args.insert("query".into(), serde_json::json!("tilo example"));
            let encoded = filter.build_json(&filter_args).unwrap();
            let started = w
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    effect_operation(&encoded.payload),
                )
                .map(Some)
                .unwrap();
            assert!(matches!(started, Some(ui::framework::Effected::Done(_))));
            assert_eq!(w.library_filter.get(), "tilo example");
        }

        // Phase `stale_blank_completion_does_not_finish_restored_page`.
        {
            struct Events(
                std::sync::Mutex<Vec<WebViewEvent>>,
                std::sync::Arc<TestEngine>,
            );
            impl WebViewEngine for Events {
                fn send(&self, command: WebViewCommand) {
                    self.1.send(command);
                }
                fn frames(&self) -> ui::webview::FrameSlot {
                    self.1.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.0.lock().unwrap())
                }
            }
            let engine = std::sync::Arc::new(Events(
                std::sync::Mutex::new(vec![
                    WebViewEvent::UriChanged("https://example.test/saved".into()),
                    WebViewEvent::LoadProgress(0.3),
                    WebViewEvent::LoadFinishedAt {
                        uri: "about:blank".into(),
                    },
                ]),
                TestEngine::new(),
            ));
            let mut widget = BrowserWidget::with_engine(engine.clone(), true)
                .with_initial_uri("https://example.test/saved".into());
            widget.title = "about:blank".into();
            widget.apply_engine_events();
            assert!(
                widget.title.is_empty(),
                "a new document must not inherit the blank page title"
            );
            assert!((widget.progress - 0.3).abs() < f64::EPSILON);
            engine.0.lock().unwrap().push(WebViewEvent::LoadFinishedAt {
                uri: "https://example.test/saved".into(),
            });
            widget.apply_engine_events();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        }

        // Phase `continuation_restore_navigates_the_matching_browser_instance`.
        {
            let engine = TestEngine::new();
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            let (mut doc, root) = synced(&mut widget);
            let metadata = ui::collaboration::ContinuationMetadata {
                schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
                bytes: br#"{"url":"https://example.test/reading"}"#.to_vec(),
            };
            assert!(widget.restore_continuation_metadata(
                &metadata,
                &mut Edits::lend(&mut doc),
                &root
            ));
            assert_eq!(
                engine.current_uri().as_deref(),
                Some("https://example.test/reading")
            );
        }
    }

    /// History pause, clear and sensitive local effects.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn history_privacy_effects_are_local_truthful_and_recoverable() {
        // Phase `sensitive_local_effects_publish_truthful_contracts_and_completed_results`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut doc, root) = synced(&mut w);
            w.history.record_at("https://private.example", "Private", 1);
            let resources = [w.published()];
            let browser = &resources[0];
            let history = browser
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.history")
                .unwrap();
            let clear_history = history
                .actions
                .iter()
                .find(|action| action.id == "clear-history")
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
                    .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY),
                "only the person at this device clears its history"
            );
            assert_eq!(
                clear_history.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(clear_history.contract.validate_declaration().is_empty());

            let invocation = ui::tilo_actions::InvocationId::new();
            let started = w
                .admit(invocation, BrowserOperation::ClearHistory)
                .map(Some)
                .unwrap();
            let Some(ui::framework::Effected::Done(output)) = started else {
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
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    BrowserOperation::RequestWebsiteDataClear,
                )
                .map(Some)
                .unwrap();
            let Some(ui::framework::Effected::Done(output)) = started else {
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

        // Phase `history_pause_is_local_and_keeps_existing_history_searchable_and_clearable`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let (mut doc, root) = synced(&mut w);
            w.history
                .record_at("https://sensitive.example", "Sensitive", 1);
            let resources = [w.published()];
            let pause = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.session")
                .unwrap();
            assert_eq!(pause.kind, "browser.history-pause");
            let toggle = pause
                .actions
                .iter()
                .find(|action| action.id == "toggle-pause")
                .unwrap();
            assert_eq!(toggle.kind, OperationKind::Effect);
            assert_eq!(toggle.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
            assert!(
                toggle
                    .contract
                    .tags
                    .iter()
                    .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY)
            );
            assert!(toggle.contract.validate_declaration().is_empty());

            let started = w
                .admit(
                    ui::tilo_actions::InvocationId::new(),
                    BrowserOperation::ToggleHistoryPause,
                )
                .map(Some)
                .unwrap();
            let Some(ui::framework::Effected::Done(output)) = started else {
                panic!("history pausing changes only this running browser");
            };
            assert!(toggle.contract.result.validate(&output).is_ok());
            let resources = [w.published()];
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
                    .find(|action| action.id == "clear-history")
                    .is_some_and(|action| action.contract.availability.is_available()),
                "existing durable history remains clearable while recording is paused"
            );
            w.search_index.sync(&w.history);
            assert!(
                ui::search::SearchProvider::search(&w.search_index.provider(), "Sensitive")
                    .iter()
                    .any(|result| result.title == "Sensitive")
            );
        }
    }

    /// Bookmark removal and row control identities.
    #[test]
    fn bookmark_rows_are_reversible_uniquely_addressed_actions() {
        // Phase `bookmark_row_controls_use_unique_normalized_identities`.
        {
            fn test_ids(widget: &dyn Widget, into: &mut Vec<String>) {
                if let Some(id) = widget.test_id() {
                    into.push(id.to_owned());
                }
                for child in widget.children() {
                    test_ids(child, into);
                }
            }
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.debug_show_library("bookmarks");
            let urls = [
                "https://www.rust-lang.org/learn",
                "https://developer.mozilla.org/",
                "https://legacy.example/",
            ];
            w.present_state(&bookmarked(&urls.map(|url| (url, url))));
            let (mut doc, root) = synced(&mut w);
            let mut tree = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

            let mut ids = Vec::new();
            test_ids(&w, &mut ids);
            let removes: Vec<&String> = ids.iter().filter(|id| id.ends_with(".remove")).collect();
            assert_eq!(removes.len(), urls.len());
            for at in 0..urls.len() {
                let id = ui::tilo_resource::Id::from_raw(format!("b{at}"));
                let control = format!("{}.remove", bookmark_resource_id(&id));
                assert!(
                    removes.iter().any(|id| **id == control),
                    "each row needs a durable, unique semantic key: {control}"
                );
            }

            let mut semantic = ui::a11y_tree::SemanticTree::new();
            semantic.refresh(&w, &node_map, None, &[]);
            assert!(
                semantic.duplicate_ids().is_empty(),
                "bookmark controls must not collapse in the retained a11y tree"
            );

            // The composed tree is not otherwise synchronized in this layout-only
            // regression test; keep the document borrow explicitly live.
            w.sync(&doc, &root);
        }
    }

    /// Download portal cancellation, worker failure, duplicate ids and scheme rejection.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn downloads_cancel_reject_and_keep_stable_actions() {
        // Phase `portal_cancel_cancels_the_paused_engine_download`.
        {
            ui::host::install_default(ui::host::HostServices::recording().0);
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

        // Phase `portal_worker_failure_cancels_webkit_but_preserves_the_actionable_error`.
        {
            ui::host::install_default(ui::host::HostServices::recording().0);
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

        // Phase `duplicate_engine_ids_are_cancelled_without_replacing_history`.
        {
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

        // Phase `download_operations_reject_non_web_schemes_and_materialize_stable_actions`.
        {
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
                .resource_by_id("browser.downloads/review-complete")
                .expect("download resource");
            assert_eq!(resource.kind, "browser.download");
            assert_eq!(
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["show-download", "open-download", "reveal-download"]
            );
            for action in &resource.actions {
                assert_eq!(action.locus, ui::tilo_actions::ExecutionLocus::ClientLocal);
            }
        }
    }

    /// Private mode personal state, indicator and discovery-failure cleanup.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn private_mode_is_empty_indicated_and_leaves_no_root() {
        // Phase `private_mode_has_empty_personal_state_and_no_continuation_channel`.
        {
            let mut widget = BrowserWidget::with_engine_mode(
                TestEngine::new(),
                false,
                crate::browsing_mode::BrowsingMode::Private,
            );
            assert!(widget.history.is_empty());
            assert!(widget.bookmarks.entries().is_empty());
            assert_eq!(widget.settings, Settings::default());
            widget
                .history
                .record_at("https://private.test", "Private", 1);
            widget.clear_history_durably().unwrap();
            assert!(widget.history.is_empty());
            let mut document = AutoCommit::new();
            assert!(
                widget
                    .continuation_metadata(&document, &automerge::ROOT)
                    .is_none()
            );
            let metadata = ui::collaboration::ContinuationMetadata {
                schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
                bytes: br#"{"url":"https://normal.test"}"#.to_vec(),
            };
            assert!(!widget.restore_continuation_metadata(
                &metadata,
                &mut Edits::lend(&mut document),
                &automerge::ROOT,
            ));
        }

        // Phase `private_indicator_action_stays_visible_in_compact_chrome`.
        {
            let mut widget = BrowserWidget::with_engine_mode(
                TestEngine::new(),
                true,
                crate::browsing_mode::BrowsingMode::Private,
            );
            widget.debug_set_viewport_width(390);
            layout_at(&mut widget, 390.0, 844.0);
            let indicator = widget.find_rect("browser.private-indicator").unwrap();
            assert!(indicator.width() > 0.0 && indicator.height() > 0.0);
            // The mark lives in the tinted toolbar; no banner takes a row
            // from the page.
            let toolbar = widget.find_rect("browser.toolbar").unwrap();
            assert_eq!(toolbar.intersect(indicator), indicator);
            let surface = widget.find_rect("browser.surface").unwrap();
            assert!(
                (surface.y0 - toolbar.y1).abs() < 1.0,
                "the page starts right under the toolbar: {toolbar:?} {surface:?}"
            );
        }

        // Phase `private_discovery_failure_filter_worker_keeps_removed_root_absent`.
        #[cfg(unix)]
        {
            use std::io::{Read, Write};

            let fixture = tempfile::tempdir().unwrap();
            let isolated_executable = fixture.path().join("isolated/bin/browser-test");
            std::fs::create_dir_all(isolated_executable.parent().unwrap()).unwrap();
            std::fs::copy(std::env::current_exe().unwrap(), &isolated_executable).unwrap();
            let done = fixture.path().join("filter-loader.done");
            let root = crate::storage::allocate_private_root().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/filters", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = tilo_test_env::hang_deadline();
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(ui::time::now() < deadline, "filter worker did not connect");
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(error) => panic!("filter endpoint failed: {error}"),
                    }
                };
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let body = b"||private-discovery.fixture^\n";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            });
            let output = std::process::Command::new(&isolated_executable)
                .args([
                    "--exact",
                    "browser_widget::tests::private_discovery_failure_filter_worker_does_not_recreate_root_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env_remove("TILO_CHROMIUM_HOST")
                .env("TILO_TEST_PRIVATE_ROOT", &root)
                .env("TILO_TEST_FILTER_LIST_URL", url)
                .env("TILO_TEST_FILTER_LOADER_DONE", &done)
                .output()
                .unwrap();
            server.join().unwrap();
            let remained = root.exists();
            if remained {
                std::fs::remove_dir_all(&root).unwrap();
            }
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!remained, "child left or recreated its private root");
        }
    }

    #[test]
    #[ignore = "child entry point for private discovery/filter lifecycle regression"]
    #[cfg(unix)]
    fn private_discovery_failure_filter_worker_does_not_recreate_root_child() {
        let root = std::path::PathBuf::from(
            std::env::var_os("TILO_TEST_PRIVATE_ROOT").expect("private root"),
        );
        let done = std::path::PathBuf::from(
            std::env::var_os("TILO_TEST_FILTER_LOADER_DONE").expect("completion marker"),
        );
        crate::storage::configure(root.to_str()).unwrap();
        let mut widget = BrowserWidget::new_with_mode(crate::browsing_mode::BrowsingMode::Private);
        assert!(
            !root.exists(),
            "Chromium discovery failure must remove the allocated root"
        );
        let mut document = AutoCommit::new();
        widget.sync(&document, &automerge::ROOT);
        let deadline = tilo_test_env::hang_deadline();
        while !done.exists() && ui::time::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(done.exists(), "legacy filter worker did not finish");
        drop(widget);
        assert!(
            !root.exists(),
            "completed unavailable-engine filter bootstrap recreated the private root"
        );
    }
}

impl BrowserWidget {
    pub(crate) fn clear_history_on_workspace_exit(&self) {
        if self.settings.clear_on_exit
            && let Some(record) = &self.person_history
            && let Err(error) =
                record.invoke(&crate::history::BrowsingHistory::CLEAR_HISTORY.with(()))
        {
            tracing::error!(%error, "Could not clear browser history on exit");
        }
    }
}

impl ui::widget::search_field::Filterable for BrowserWidget {
    fn set_filter(&mut self, query: &str) {
        self.library_filter.set(bounded_library_filter(query));
        self.show_library = true;
        self.show_settings = false;
        self.structure_dirty = true;
    }
    fn search_scope(&self) -> Option<ui::search::ScopeDecl> {
        let (id, label) = match self.library_section {
            LibrarySection::History => (crate::actions::HISTORY_SCOPE, "History"),
            LibrarySection::Bookmarks => (crate::actions::BOOKMARKS_SCOPE, "Bookmarks"),
            LibrarySection::Downloads => (crate::actions::DOWNLOADS_SCOPE, "Downloads"),
            LibrarySection::Extensions => ("browser.extensions", "Extensions"),
        };
        Some(ui::search::ScopeDecl::new(id, label))
    }
}

#[derive(Clone, Copy, Debug)]
enum ReviewStage {
    Autofill { addresses: bool },
}

fn is_compact_chrome(width: f64, breakpoint: f32) -> bool {
    width < f64::from(breakpoint)
}

impl BrowserWidget {
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.dispatch.is_some()
    }
    fn route_document_button(
        &self,
        button: Button,
        _resource: &str,
        _action_id: &str,
        action: Option<Invoke<Browser>>,
    ) -> Button {
        self.document_button(button, action)
    }
}
