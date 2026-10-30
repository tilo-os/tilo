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
use ui::tilo_actions::{ActionBinding, ControlInvocation, ResourceSpec};
use ui::webview::test_engine::TestEngine;
use ui::webview::{
    MediaElementInfo, NavigationCommand, NavigationDirection, PageFailure, PageReply, PageRequest,
    SharedEngine, WebViewCommand, WebViewEvent,
};
use ui::widget::{
    AppKind, AppWidget, Button, Column, Container, ContextMenu, Continuable, EmptyState,
    FloatingSurfaceProvider, IconDescriptor, MenuItem, Popover, RuleEdge, WebView, Widget,
    paint_rule,
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
use load_failure::AddressProblem;
use ui::lock::MutexRecover as _;
use ui::tilo_resource::Invoke;
use ui::tilo_resource::Resource as _;

#[cfg(test)]
mod action_surface_tests;
mod address_rows;
#[cfg(test)]
mod architecture_tests;
mod continuity;
mod dialog;
mod download_read;
mod engine_runtime;
mod http_fallback;
mod library;
#[cfg(test)]
#[allow(
    unused_mut,
    unused_variables,
    reason = "legacy layout helpers retain state handles for parity with live setup"
)]
mod library_tests;
mod load_failure;
#[cfg(test)]
mod page_actions;
mod page_answer;
mod page_clipboard;
mod page_find;
mod page_stopping;
pub(crate) mod personal;
#[cfg(test)]
mod regression_tests;
mod review_hooks;
mod side_panel;
#[cfg(test)]
#[allow(
    unused_mut,
    unused_variables,
    reason = "legacy layout helpers retain state handles for parity with live setup"
)]
mod tests;
mod toolbar;
mod window_bar;

#[cfg(test)]
pub(crate) use engine_runtime::test_window_engine;
use engine_runtime::{default_engine, engine_failed_state};
#[cfg(test)]
use engine_runtime::{engine_or_error, unavailable_engine};

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

/// A failure one of this app's invocations reports.
///
/// A retryable failure is only legal under a contract that permits a retry,
/// and the broker refuses one reported under any other: it then drops the
/// whole collaboration bridge, and from that moment the tab answers every
/// later action with "outcome unknown" for the rest of the run.  Rather than
/// keep that decision beside each failure, every failure this app reports is
/// built here and claims no retry: a caller that wants to act again does so
/// as a new invocation, which is what the at-most-once page actions require
/// and what the idempotent ones do not mind.
fn action_failed(
    invocation: ui::tilo_actions::InvocationId,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Option<serde_json::Value>,
) -> ui::tilo_actions::InvocationUpdate {
    ui::tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: ui::tilo_actions::ActionFailure {
            code: code.into(),
            message: message.into(),
            details,
            retryable: false,
        },
    }
}

/// A page action admitted through the resource surface.
struct PageOperation {
    invocation: ui::tilo_actions::InvocationId,
    request_id: u32,
    /// The action as it was asked, for an answer that says what ran: a
    /// capture's picture is an artifact, a read's is the page to look at.
    acted: ui::webview::PageRequest,
    phase: PagePhase,
    deadline: std::time::Instant,
    /// A main-frame load that settled while the request was with the engine
    /// and how. The engine answers a navigating action only once its page
    /// loaded, after the tab heard so: an answer that says "loading" then
    /// completes at once instead of waiting for a load that already happened.
    settled_while_sent: Option<SettledLoad>,
    /// What the request does, repeated in an answer a question it opened cut
    /// short: `Clicked element d1e5`.
    act: String,
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

/// Where choosing omnibox `row` goes: a search row's engine page (searching
/// with `engine`) for its words, else its address.
pub(super) fn row_destination(row: &omnibox::Row, engine: &str) -> String {
    match row.kind {
        omnibox::RowKind::Search | omnibox::RowKind::Suggestion => search_url(engine, &row.url),
        omnibox::RowKind::Go
        | omnibox::RowKind::History
        | omnibox::RowKind::Bookmark
        | omnibox::RowKind::Extension => row.url.clone(),
    }
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
/// scheme/port and is not a browser address syntax. The omnibox's own test,
/// so Enter opens what the address list's first row says: `U.S.` or
/// `foo/bar.baz` is a search there, and was an address here.
fn looks_like_bare_web_host(value: &str) -> bool {
    omnibox::looks_like_address(value)
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

/// `uri` without its `#fragment`: the document it names.
fn without_fragment(uri: &str) -> &str {
    uri.split_once('#').map_or(uri, |(document, _)| document)
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
    Popup,
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

pub(crate) use crate::rows::library_filter_matches;

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
    use ui::i18n::ArgValue;
    let catalog = &crate::resource::CATALOG;
    let size = |bytes: u64| ArgValue::Text(ui::format::size(bytes));
    match entry.status {
        DownloadStatus::AwaitingDestination => catalog.t("browser-download-waiting-destination"),
        DownloadStatus::Downloading => match entry.total_bytes {
            Some(total) => catalog.t_args(
                "browser-download-progress",
                &[
                    ("received", size(entry.received_bytes)),
                    ("total", size(total)),
                ],
            ),
            None => catalog.t_args(
                "browser-download-received",
                &[("received", size(entry.received_bytes))],
            ),
        },
        DownloadStatus::Completed => entry.destination.as_deref().map_or_else(
            || catalog.t("browser-download-complete"),
            |path| {
                catalog.t_args(
                    "browser-download-saved-to",
                    &[("path", ArgValue::Text(path.display().to_string()))],
                )
            },
        ),
        // The engine's own words for what went wrong, when it said; the
        // catalogue's when the download failed without a reason.
        DownloadStatus::Failed => match entry.error.as_deref() {
            Some(error) => error.to_owned(),
            None => catalog.t("browser-download-failed"),
        },
        DownloadStatus::Cancelled => catalog.t("browser-download-cancelled"),
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
    /// The extension whose side panel shows, whether it docks, and how wide
    /// it stands.
    side_panel: Option<(String, bool, f32)>,
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
    /// Where a picture of the page is kept: content the agent's answer names
    /// by hash. `None` until a host attaches one, so a bare widget pictures
    /// nothing and says so.
    content: Option<ui::content::ContentStore>,
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
    /// The page's popup windows (a sign-in), shown in the panel.
    page_popups: crate::page_popups::PagePopups,
    /// The question the page waits on (`alert`, `confirm`, `prompt`,
    /// "leave this page?"), shown over the window, and its dialog's node.
    page_dialogs: crate::page_dialogs::PageDialogs,
    page_dialog_node: Option<NodeId>,
    /// What answering the page's question did, written when it is answered
    /// so the action's answer says what happened next.
    page_dialog_answer: Option<String>,
    /// What answering a site's permission request did, written when it is
    /// answered so the action's answer says what happened next.
    permission_answer: Option<String>,
    credentials: crate::credentials::Credentials,
    /// The popover open next to the toolbar control it belongs to: the site
    /// or autofill panel, a permission or download question, or the no-video
    /// note.
    anchored_panel: Option<Box<dyn Widget>>,
    /// The edge between the page and a docked side panel, which the person
    /// drags to widen the panel (`side_panel.rs`), and its layout node: kept
    /// across layouts so a drag goes on, replaced only in `build_layout`.
    side_panel_edge: Option<Box<ui::widget::SplitHandle>>,
    side_panel_edge_node: Option<NodeId>,
    /// How the edge was last drawn (lit, where), to repaint it when that
    /// changes.
    side_panel_edge_drawn: Option<(bool, Rect)>,
    panel_dismissed: ui::LocalState<Option<PanelKind>>,
    pip_choice: ui::LocalState<Option<PipChoice>>,
    pending_chrome_focus: Option<(NodeId, &'static str)>,
    private_window_request: ui::LocalState<bool>,
    /// The window bar's entries, each carrying what it runs.
    bar_entries: ui::framework::BarCommands<window_bar::BarCommand>,
    private_window_notice: Option<String>,
    /// The device scale the page lands on screen with, as of the last draw:
    /// what a rebuilt page view starts at. The view keeps its own scale
    /// from then on (`ui::widget::WebView`, "Device scale"); input never
    /// sets either.
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
    /// The address's text as the extensions' keywords last saw it, whether
    /// it then followed one (the rows are the extension's), and whether the
    /// extension's rows changed since they were shown.
    keyword_typed: String,
    keyword_active: bool,
    keyword_dirty: bool,
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
    /// The page last recorded in [`Self::person_history`] before it had a
    /// title, to name once its title comes.
    untitled_visit: Option<String>,
    continuity: Option<crate::continuity_handoff::Controller>,
    /// The person's address book, which new tabs fill forms from too.
    address_book: Option<ui::framework::PersonRecord<crate::autofill::AddressBook>>,
    /// The record's revision [`Self::history`] shows.
    history_revision: Option<u64>,
    history: History,
    bookmarks: Bookmarks,
    downloads: Downloads,
    download_picker: Option<(String, ui::portal::PortalChannel)>,
    page_clipboard: page_clipboard::PageClipboard,
    download_picker_queue: std::collections::VecDeque<(String, String)>,
    download_save_due: Option<ui::time::Instant>,
    /// This window's engine session, which keys its downloads in the saved
    /// list: the engine numbers them from 1 again at every start.
    download_session: String,
    /// Retained full-corpus index. Provider handles are cheap and unchanged
    /// frames compare only the durable corpus revision tokens.
    search_index: crate::search_provider::BrowserSearchIndex,
    /// Fuzzy suggestion dropdown under the address bar (a field, not a
    /// tree child: driven here and painted in `draw_overlay`). Empty while
    /// the window's bar hosts the address: the bar lists `bar_rows` then,
    /// and there is never a second list.
    suggestions: AddressSuggestions,
    /// The omnibox's rows for what is typed in the window's bar, which the
    /// bar lists under its address (`window_bar`).
    bar_rows: Vec<omnibox::Row>,
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
    /// Saved pages prepared for per-keystroke matching, beside the visits.
    omni_bookmarks: omnibox::BookmarkIndex,
    /// The indexes must be rebuilt: the history or the bookmarks changed
    /// since they were built.
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
    /// Where the typed address now opening over `https://` goes if that
    /// fails (`http_fallback`).
    http_fallback: Option<http_fallback::HttpFallback>,
    explicit_navigation_requested: bool,
    resource_navigation_result: Option<ui::tilo_actions::InvocationUpdate>,
    /// Navigations a later one replaced, settled and not yet read.
    replaced_navigations: Vec<ui::tilo_actions::InvocationUpdate>,
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
    engine_failed: bool,
    /// A credential was entered into this window's page: the person typed
    /// into a password field, or a secret was filled in for them. From then
    /// on the page holds a sign-in, whatever the window opened as. Only ever
    /// set, never cleared: a page signed in stays one the window must treat
    /// as signed in.
    sign_in_entered: bool,
}

impl BrowserWidget {
    /// Live-mode widget on the Chromium engine runtime. When the engine does
    /// not start the page area says so.
    pub fn new_with_mode(mode: BrowsingMode) -> Self {
        let (engine, failed) = default_engine(mode);
        let mut widget = Self::with_engine_mode(engine, false, mode);
        widget.engine_failed = failed;
        widget
    }

    /// Headless widget for `--shots`: test engine, pre-navigated to a
    /// deterministic page, never touches the network.
    pub fn new_headless() -> Self {
        Self::new_headless_in(BrowsingMode::Normal)
    }

    /// [`Self::new_headless`] for a window of `mode`: a capture of a
    /// visitor window shows its chrome.
    pub fn new_headless_in(mode: BrowsingMode) -> Self {
        let engine: SharedEngine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(SHOTS_URL.to_string()));
        let mut w = Self::with_engine_mode(engine, true, mode);
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
        if self.browsing_mode.holds_account_custody()
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
    /// How this window browses, fixed when it opened.
    pub(crate) const fn browsing_mode(&self) -> BrowsingMode {
        self.browsing_mode
    }

    /// Whether the page may hold a sign-in of the person's: always in their
    /// own window, and in one opened as a visitor once a credential was
    /// entered there ([`Self::hold_sign_in`]). Reading such a page takes the
    /// authority to operate it.
    pub(crate) const fn page_holds_sign_in(&self) -> bool {
        self.browsing_mode.holds_account_custody() || self.sign_in_entered
    }

    /// The page holds a sign-in from now on: a credential was entered into
    /// it. One-way; the window republishes its page actions as what they
    /// now are, and its bar stops saying it is not signed in.
    pub(crate) fn hold_sign_in(&mut self) {
        if !self.sign_in_entered {
            self.sign_in_entered = true;
            self.structure_dirty = true;
        }
    }

    /// A page action that typed (`fill-page`, `press-page`, a secret) has
    /// ended, however it ended: if the field it typed into is a password
    /// field, a credential was entered and the page holds a sign-in. The
    /// field's type decides, not the action: the engine reports the focused
    /// field as the action focuses it, before it answers, so the field
    /// reported now is the one the keys went to.
    fn note_text_entered(&mut self, acted: &PageRequest) {
        if acted.enters_text() && self.focused_field_is_password() {
            self.hold_sign_in();
        }
    }

    /// Whether the page's focused field, as the engine reports it, is a
    /// password field.
    fn focused_field_is_password(&self) -> bool {
        self.engine
            .text_field()
            .is_some_and(|field| field.purpose == ui::ime::InputPurpose::Password)
    }

    /// Whether `event`, reaching the page, is the person typing into the
    /// page's focused password field: entering a credential. Moving between
    /// fields, modifiers alone and Escape enter nothing.
    fn types_a_password(&self, event: &WindowEvent) -> bool {
        let typing = match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                key.state == ElementState::Pressed
                    && !matches!(
                        key.logical_key,
                        WKey::Named(
                            WNamedKey::Tab
                                | WNamedKey::Escape
                                | WNamedKey::Shift
                                | WNamedKey::Control
                                | WNamedKey::Alt
                                | WNamedKey::Super
                                | WNamedKey::Meta
                        )
                    )
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => !text.is_empty(),
            _ => false,
        };
        typing && self.control_focused("browser.surface") && self.focused_field_is_password()
    }

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

    /// Keep the widths the person drags extensions' side panels to in this
    /// device's settings, starting from those kept there.
    pub(crate) fn attach_side_panel_widths(
        &mut self,
        store: ui::storage::Settings<crate::extension_controller::SidePanelWidths>,
    ) {
        self.extensions.attach_side_panel_widths(store);
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
            // The address bar suggests the bookmarks this window shows.
            self.omni_stale = true;
            self.structure_dirty = true;
        }
    }
    /// The content store a picture of the page is kept in: opaque bytes by
    /// hash, which the agent's answer names, so what a page shows as pixels
    /// reaches the reader who can see them. The store the host lent the
    /// window (`RecordEnv::content`), so the bytes land where the content
    /// service reads them from.
    pub(crate) fn attach_content(&mut self, content: ui::content::ContentStore) {
        self.content = Some(content);
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
            content: None,
            site_exceptions: std::collections::BTreeSet::new(),
            address: ui::LocalState::new(initial_url.clone()),
            address_navigation_value: None,
            daily: crate::daily_controls::DailyControls::default(),
            page_menu: crate::page_menu::PageMenu::default(),
            site: crate::site_controls::SiteControls::default(),
            page_popups: crate::page_popups::PagePopups::default(),
            page_dialogs: crate::page_dialogs::PageDialogs::default(),
            page_dialog_node: None,
            page_dialog_answer: None,
            permission_answer: None,
            // Saved passwords and addresses are the person's account
            // custody: a window without it neither offers nor saves them.
            credentials: crate::credentials::Credentials::new(
                !browsing_mode.holds_account_custody(),
            ),
            anchored_panel: None,
            side_panel_edge: None,
            side_panel_edge_node: None,
            side_panel_edge_drawn: None,
            panel_dismissed: ui::LocalState::new(None),
            pip_choice: ui::LocalState::new(None),
            pending_chrome_focus: None,
            private_window_request: ui::LocalState::new(false),
            bar_entries: ui::framework::BarCommands::new(),
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
            keyword_typed: String::new(),
            keyword_active: false,
            keyword_dirty: false,
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
            untitled_visit: None,
            continuity: None,
            address_book: None,
            history_revision: None,
            history,
            bookmarks,
            downloads,
            download_picker: None,
            page_clipboard: page_clipboard::PageClipboard::default(),
            download_picker_queue: std::collections::VecDeque::new(),
            download_save_due: None,
            download_session: crate::downloads::new_session(),
            search_index,
            suggestions: AddressSuggestions::new(),
            bar_rows: Vec::new(),
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
            omni_bookmarks: omnibox::BookmarkIndex::default(),
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
            http_fallback: None,
            explicit_navigation_requested: false,
            resource_navigation_result: None,
            replaced_navigations: Vec::new(),
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
            engine_failed: false,
            sign_in_entered: false,
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
            // The address bar suggests what was saved too.
            self.omni_stale = true;
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

    /// Document edits bound to the page's row: it turns tracker protection
    /// on or off for the site it shows.
    fn page_row_declarations(&self) -> Vec<ui::tilo_resource::ActionDecl<Browser>> {
        let Some(origin) = crate::site_controls::origin(&self.current_uri) else {
            return Vec::new();
        };
        let protected = !self.site_exceptions.contains(&origin);
        Browser::site_switch(&origin, protected)
            .into_iter()
            .collect()
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
            || self.engine_failed
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
                self.expect_http_fallback(None, "");
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.send_navigation(NavigationCommand::Back, owner)?;
            }
            BrowserOperation::Forward => {
                self.expect_http_fallback(None, "");
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
                self.expect_http_fallback(None, "");
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
                    self.resource_navigation_result = Some(action_failed(
                        invocation,
                        "browser.navigation_stopped",
                        "Page loading stopped.",
                        None,
                    ));
                }
            }
            BrowserOperation::OpenInNewTab { url, background } => {
                let target = self.navigation_target(url.trim())?;
                self.open_beside(target, background);
            }
            BrowserOperation::Navigate { url } => {
                let target = self.navigation_target(url.trim())?;
                self.expect_http_fallback(Some(url.as_str()), &target);
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
            BrowserOperation::ForgetVisit { url } => {
                self.forget_visit(&url)?;
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
                let command = match self.native_download_id(&id) {
                    Some(native) => WebViewCommand::CancelNativeDownload { id: native },
                    None => WebViewCommand::CancelDownload { id },
                };
                self.engine.send(command);
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
            BrowserOperation::Extension(request) => {
                self.extensions.request(&request)?;
                self.structure_dirty = true;
            }
            BrowserOperation::AddExtension => self.extensions.offer_store_item()?,
            BrowserOperation::AnswerPageDialog {
                dialog,
                accept,
                text,
            } => {
                // What was asked, so the answer says what it did with it and
                // what to do next; the page goes on from here.
                let asked = self.page_dialogs.current().cloned();
                self.page_dialogs
                    .answer(&self.engine, dialog, accept, text.clone())?;
                self.structure_dirty = true;
                let answer = asked.as_ref().map_or_else(
                    || "Answered the page's dialog.".to_string(),
                    |asked| dialog::answered(asked, accept, text.as_deref().unwrap_or_default()),
                );
                self.page_dialog_answer = Some(answer);
            }
            BrowserOperation::AnswerPermission { request, allow } => {
                let asked = self
                    .capabilities
                    .pending()
                    .filter(|(id, _, _)| *id == request)
                    .map(|(_, origin, rights)| (origin.to_owned(), rights.to_owned()));
                self.capabilities.answer(&self.engine, request, allow)?;
                self.structure_dirty = true;
                self.permission_answer = Some(match asked {
                    Some((origin, rights)) => format!(
                        "The site {origin} {} {rights}.",
                        if allow { "may now use" } else { "may not use" }
                    ),
                    None => "Answered the site's permission request.".to_string(),
                });
            }
            BrowserOperation::Zoom { step } => {
                self.daily.zoom_by(step, &self.engine)?;
                self.structure_dirty = true;
            }
            BrowserOperation::RevealDownload { id } => {
                let path = self.completed_download_path(&id)?;
                let _ = ui::portal::LaunchRequest::reveal(path)
                    .thread_name("browser-reveal-download")
                    .send();
            }
            BrowserOperation::ReadDownload { .. } | BrowserOperation::AnswerDownload { .. } => {
                return Err("downloads read and answered through their resource action".into());
            }
            BrowserOperation::Page(_) => {
                return Err("page actions report their result; start them as an invocation".into());
            }
        }
        Ok(())
    }

    fn page_loading(&self) -> bool {
        self.progress < 1.0 && !self.loading_stopped
    }

    /// Choose files for a page upload.
    ///
    /// What an agent names here becomes files the page reads, so the rule is
    /// the same one a person's file picker applies: every path must be
    /// absolute, name a regular file this Browser may open, and the opened
    /// descriptor is the only thing that decides. A relative path, a folder,
    /// a missing file or one the sandbox refuses is an error naming that
    /// path, never a silently dropped or guessed file. Inside the launcher's
    /// sandbox the open itself is what Landlock and the mount namespace
    /// answer; without one (a standalone development run) it answers the
    /// host's own permission check. Nothing outside that rule is widened to
    /// make an upload convenient.
    fn checked_page_files(paths: &[String]) -> Result<Vec<String>, String> {
        let mut checked = Vec::with_capacity(paths.len());
        for path in paths {
            let named = std::path::Path::new(path);
            if !named.is_absolute() {
                return Err(format!(
                    "`{path}` is not an absolute path; a page file is named by its full path"
                ));
            }
            let file = ui::portal::open_regular_file(named)
                .map_err(|error| format!("{path} cannot be given to the page: {error}"))?;
            drop(file);
            checked.push(path.clone());
        }
        Ok(checked)
    }

    /// Read and operate the page's content without its pixels: an outline
    /// whose interactive elements carry refs, and actions on those refs.
    fn start_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        request: PageRequest,
    ) -> Result<(), String> {
        let request = match request {
            PageRequest::FillFiles {
                element,
                paths,
                frame,
            } => PageRequest::FillFiles {
                element,
                paths: Self::checked_page_files(&paths)?,
                frame,
            },
            other => other,
        };
        if let PageRequest::Press { key, .. } = &request
            && let Err(why) = ui::webview::PageKey::parse_detailed(&key.chord())
        {
            return Err(why);
        }
        if !request.valid() {
            if !request.frame_valid() {
                return Err(format!(
                    "`{}` was given a frame id the page outline never printed",
                    request.action()
                ));
            }
            if let PageRequest::Scroll { .. } = request {
                return Err(
                    "`scroll` goes to an end of the page, by a distance, or brings an element \
                     into view: one of them, and no two"
                        .into(),
                );
            }
            return Err(format!(
                "`{}` names no element of the page outline, or arguments the page cannot take",
                request.action()
            ));
        }
        // A settled operation's update waits here until the effect reply that
        // asked for it reads it ([`Self::settle_replies`]). One whose reply is
        // gone is spent: that caller was answered `accepted` and moved on, so
        // nothing will ever read it here (`tilo_status` still answers it), and
        // holding the slot would refuse every later action in this tab. An
        // operation still in flight is never abandoned: it settles at its own
        // deadline.
        self.abandon_unwatched_page_result();
        if self.page_operation.is_some() {
            return Err("another page action is already running in this tab".into());
        }
        // A result the caller stopped waiting for while its page action ran on
        // to completion is refused here exactly like one still running: the
        // slot frees itself when that action settles and nobody reads it.
        if self.page_operation_result.is_some() {
            return Err("another page action is already running in this tab".into());
        }
        // The page runs nothing while it waits on its question: an action
        // sent now would only time out.
        if let Some(dialog) = self.page_dialogs.current() {
            let refused = dialog::blocked_output(invocation, Some(dialog));
            self.page_operation_result = Some(refused);
            return Ok(());
        }
        // A secret filled into the page is the person's credential: the page
        // holds a sign-in from the moment it is sent.
        if matches!(request, PageRequest::Fill { secret: true, .. }) {
            self.hold_sign_in();
        }
        self.page_request_seq = self.page_request_seq % i32::MAX.unsigned_abs() + 1;
        let act = request.described();
        let acted = request.clone();
        let now = std::time::Instant::now();
        self.page_operation = Some(PageOperation {
            invocation,
            request_id: self.page_request_seq,
            acted,
            phase: PagePhase::Queued {
                request,
                until: now + PAGE_LOAD_WAIT,
            },
            deadline: now + PAGE_OPERATION_TIMEOUT,
            settled_while_sent: None,
            act,
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
        self.note_text_entered(&operation.acted);
        self.reader_request = None;
        self.page_operation_result = Some(match operation.phase {
            PagePhase::Navigating { reply } => self.page_success(
                operation.invocation,
                &reply,
                &operation.acted,
                Some("the page it opened is still loading"),
            ),
            PagePhase::Queued { .. } | PagePhase::Sent => {
                Self::answer_a_timed_out_page_operation(&operation)
            }
        });
    }

    /// The page asked a question while a page action waits on it: the page
    /// stops until the question is answered. The action itself did what it
    /// said before the page stopped to ask, so it answers as a success that
    /// carries the question, rather than as a failure of that action or as a
    /// wait until it times out.
    fn settle_page_operation_on_question(&mut self) {
        let Some(operation) = self.page_operation.take() else {
            return;
        };
        self.note_text_entered(&operation.acted);
        self.reader_request = None;
        let dialog = self.page_dialogs.current();
        // The page asked and the question went before it could be read:
        // nothing is left to answer, so the action is refused as it was.
        self.page_operation_result = Some(match dialog {
            Some(dialog) => dialog::opened_output(
                operation.invocation,
                &operation.act,
                &self.current_uri,
                &self.title,
                dialog,
            ),
            None => dialog::blocked_output(operation.invocation, None),
        });
    }

    fn accept_page_result(&mut self, request_id: u32, result: Result<PageReply, PageFailure>) {
        let Some(operation) = self.page_operation.take_if(|operation| {
            operation.request_id == request_id && matches!(operation.phase, PagePhase::Sent)
        }) else {
            return;
        };
        self.note_text_entered(&operation.acted);
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
            self.page_operation_result = Some(self.page_success(
                operation.invocation,
                reply,
                &operation.acted,
                note.as_deref(),
            ));
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
            Ok(reply) => self.page_success(operation.invocation, &reply, &operation.acted, None),
            // A page that refused the action because it was busy, or never
            // answered, is reported as what it is.  Calling it retryable here
            // would contradict the at-most-once contract this app publishes
            // and the broker would refuse the answer, taking the whole
            // session's page actions down with it.
            Err(failure) => action_failed(
                operation.invocation,
                format!("browser.page.{}", failure.code),
                failure.message,
                None,
            ),
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
        self.page_operation_result = Some(self.page_success(
            operation.invocation,
            &reply,
            &operation.acted,
            note.as_deref(),
        ));
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

    fn resource_by_id(&self, wanted: &str) -> Option<ResourceSpec> {
        fn walk(resources: &[ResourceSpec], wanted: &str) -> Option<ResourceSpec> {
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
            return self.reject_navigation(AddressProblem::Empty);
        }
        if input.len() > 8 * 1024 || input.chars().any(char::is_control) {
            return self.reject_navigation(AddressProblem::Invalid);
        }

        // An explicitly schemed address must satisfy the manifest boundary.
        // We test it before treating dotted text as a host so `file:///a.html`
        // cannot get rewritten into an apparent HTTPS address.
        if input.contains("://") {
            let Ok(url) = url::Url::parse(input) else {
                return self.reject_navigation(AddressProblem::Invalid);
            };
            if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
                return self.reject_navigation(AddressProblem::NotWeb);
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
            return self.reject_navigation(AddressProblem::NotWeb);
        }

        let candidate =
            if !input.chars().any(char::is_whitespace) && looks_like_bare_web_host(input) {
                omnibox::address_target(input)
            } else {
                search_url(&self.settings.search_engine, input)
            };
        let Ok(parsed) = url::Url::parse(&candidate) else {
            return self.reject_navigation(AddressProblem::Invalid);
        };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
            return self.reject_navigation(AddressProblem::NotWeb);
        }
        Ok(candidate)
    }

    /// Name the visit just recorded without a title, once the page it
    /// recorded has one.
    fn name_untitled_visit(&mut self) {
        let Some(title) = ui::tilo_resource::NonBlank::parse(&self.title) else {
            return;
        };
        if self.untitled_visit.as_deref() != Some(self.current_uri.as_str()) {
            return;
        }
        self.untitled_visit = None;
        if let Some(record) = &self.person_history {
            let name = crate::history::BrowsingHistory::NAME_VISIT.with(
                crate::history::browsing_history::NameVisit {
                    url: self.current_uri.clone(),
                    title,
                },
            );
            if let Err(error) = record.invoke(&name) {
                tracing::warn!(%error, "browser visit was not named");
            }
            self.follow_history();
        }
    }

    fn reject_navigation<T>(&mut self, problem: AddressProblem) -> Result<T, String> {
        let message = problem.message();
        self.show_navigation_error(&message);
        Err(message)
    }

    fn show_navigation_error(&mut self, message: &str) {
        self.progress = 1.0;
        self.title = load_failure::address_failed_title();
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
                PanelKind::Popup => self.page_popups.close(),
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
        self.resource_pip_result = Some(action_failed(invocation, code, message, None));
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
            side_panel: self.side_panel_key(theme),
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
        } else if self.engine_failed {
            engine_failed_state()
        } else if let Some(error) = &self.load_error {
            let heading = if self.address_error_active {
                load_failure::address_failed_title()
            } else {
                load_failure::page_failed_title()
            };
            let recovery = EmptyState::error(heading, error)
                .with_icon(Icon::Wifi)
                .with_action(crate::resource::CATALOG.t("browser-page-retry"))
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
            self.page_beside_side_panel(page_name, theme)
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
        self.page_clipboard.answer_ready(&*self.engine);
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
                event @ (WebViewEvent::ExtensionAction { .. }
                | WebViewEvent::ExtensionActionReset
                | WebViewEvent::ExtensionMenu { .. }
                | WebViewEvent::ExtensionOpenPopup { .. }
                | WebViewEvent::ExtensionExecuteAction { .. }
                | WebViewEvent::ExtensionNotification { .. }
                | WebViewEvent::ExtensionPermissionRequest { .. }
                | WebViewEvent::ExtensionPermissions { .. }
                | WebViewEvent::ExtensionSignIn { .. }
                | WebViewEvent::ExtensionSidePanel { .. }
                | WebViewEvent::ExtensionShortcuts(_)) => {
                    self.structure_dirty |= self.extensions.on_engine_event(event, &self.engine);
                }
                // What an extension suggests for its keyword in the address.
                event @ (WebViewEvent::ExtensionOmniboxSuggestions { .. }
                | WebViewEvent::ExtensionOmniboxDefault { .. }) => {
                    self.keyword_dirty |= self.extensions.on_engine_event(event, &self.engine);
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
                | WebViewEvent::ExtensionPermissionClosed { .. }
                | WebViewEvent::NativeDownloadRequested { .. }
                | WebViewEvent::DownloadUpdated { .. }
                | WebViewEvent::DownloadRuntime { .. }
                | WebViewEvent::ExtensionDownloadOpen { .. }) => {
                    self.keep_native_download(&event);
                    self.capabilities.apply(event);
                    self.structure_dirty = true;
                }
                WebViewEvent::UriChanged(uri) => {
                    self.follow_http_fallback(&uri);
                    self.site.navigation(&uri);
                    let requests = self.credentials.navigation(&uri);
                    crate::credentials::Credentials::send(&self.engine, requests);
                    self.structure_dirty = true;
                    self.document_revision = self.document_revision.wrapping_add(1);
                    // A jump to a `#section` of the page is the same document:
                    // its title, icon, media, find and reader view stay.
                    let same_document = uri != self.current_uri
                        && without_fragment(&uri) == without_fragment(&self.current_uri);
                    if !same_document {
                        self.favicon = None;
                        self.favicon_png = None;
                        self.media_playing = false;
                        self.media_document_generation = None;
                    }
                    if uri != self.current_uri && !same_document {
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
                    self.name_untitled_visit();
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
                WebViewEvent::ClipboardWrite { text, html, secret } => {
                    page_clipboard::PageClipboard::copied(&text, html.as_deref(), secret);
                }
                WebViewEvent::ClipboardRead { request_id } => {
                    self.page_clipboard.read(request_id);
                    self.page_clipboard.answer_ready(&*self.engine);
                }
                WebViewEvent::FullscreenChanged(active) => {
                    if self.content_fullscreen != active {
                        self.content_fullscreen = active;
                        self.structure_dirty = true;
                    }
                }
                WebViewEvent::RendererTerminated => {
                    // The page's renderer is gone (out of memory, a crash):
                    // its last frame is no page any more. Say so, with the
                    // way back a failed load offers, instead of a page that
                    // looks open and takes no input.
                    self.set_loading_progress(1.0);
                    self.load_error = Some(crate::resource::CATALOG.t("browser-page-stopped"));
                    self.structure_dirty = true;
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
                | WebViewEvent::DocumentSize { .. }
                | WebViewEvent::SelectionText { .. }
                | WebViewEvent::LinkHovered(_)
                | WebViewEvent::FindSelection { .. } => {}
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
                    self.daily.page_loaded();
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if self.browsing_mode.persists_personal_state()
                        && !self.history_paused()
                        && self.current_uri.starts_with("http")
                        && let Some(url) = ui::tilo_resource::NonBlank::parse(&self.current_uri)
                        && let Some(record) = &self.person_history
                    {
                        let visit = crate::history::BrowsingHistory::VISIT.with(
                            crate::history::browsing_history::Visit {
                                url,
                                title: self.title.clone(),
                                at: tilo_clock::unix_now(),
                            },
                        );
                        if let Err(error) = record.invoke(&visit) {
                            tracing::warn!(%error, "browser visit was not recorded");
                        }
                        // A page a web app moves to is named after it loads.
                        self.untitled_visit = self
                            .title
                            .trim()
                            .is_empty()
                            .then(|| self.current_uri.clone());
                        self.follow_history();
                        self.follow_personal_bookmarks();
                    }
                    self.page_navigation_settled(None);
                }
                WebViewEvent::LoadFailed { uri, error } => {
                    if self.retry_over_http(&uri, &error) {
                        continue;
                    }
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
                    self.title = load_failure::page_failed_title();
                    self.load_error = Some(load_failure::page_failed_message(&error));
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
                            Some(message) => action_failed(
                                invocation,
                                "browser.navigation_failed",
                                message,
                                Some(serde_json::json!({ "uri": uri })),
                            ),
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
                event @ (WebViewEvent::PageDialog(_) | WebViewEvent::PageDialogClosed { .. }) => {
                    // The question is taken first, so the action it stopped
                    // answers with it rather than with a question already gone.
                    self.structure_dirty |= self.page_dialogs.apply(&event);
                    if matches!(event, WebViewEvent::PageDialog(_)) {
                        self.settle_page_operation_on_question();
                    }
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

    /// The saved list's key for the engine's download `id`.
    fn native_download_key(&self, id: u32) -> String {
        format!("native-{}-{id}", self.download_session)
    }

    /// The engine's download `id` a saved list `key` names, when it is one
    /// of this session's.
    fn native_download_id(&self, key: &str) -> Option<u32> {
        key.strip_prefix("native-")?
            .strip_prefix(self.download_session.as_str())?
            .strip_prefix('-')?
            .parse()
            .ok()
    }

    /// Keep an engine download in the saved list, which the Library shows
    /// after a restart and search, agents and the download actions read: the
    /// engine's own list lives as long as the engine.
    fn keep_native_download(&mut self, event: &WebViewEvent) {
        // An offer the engine asks the person to answer becomes a row too, so
        // its Save and Discard ride the download the agent already finds.
        if let WebViewEvent::NativeDownloadRequested { id, url, name } = event {
            let key = self.native_download_key(*id);
            if self.downloads.get(&key).is_none()
                && self.downloads.requested(key, url.clone(), name)
            {
                self.persist_downloads_now();
                self.structure_dirty = true;
            }
            return;
        }
        let WebViewEvent::DownloadUpdated {
            id,
            state,
            path,
            name,
            url,
            received,
            total,
            error,
            ..
        } = event
        else {
            return;
        };
        let key = self.native_download_key(*id);
        // A page's own export (`data:`, `blob:`) has no address to fetch again,
        // but the bytes it did write are a file the person may keep and read.
        if self.downloads.get(&key).is_none()
            && !self.downloads.requested(key.clone(), url.clone(), name)
        {
            return;
        }
        match state.as_str() {
            "progress" => {
                // Until the person says where the download goes, the engine
                // already reports its bytes while nothing is kept. Keep the
                // row offering Save and Discard rather than showing a transfer
                // that cannot finish or be read.
                if self.capabilities.offer_pending(*id) && self.downloads.awaiting_offer(&key) {
                    return;
                }
                if !path.is_empty() {
                    self.downloads.destination(&key, path.into());
                }
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a fraction of a download only draws a progress bar"
                )]
                let fraction = if *total > 0 {
                    *received as f64 / *total as f64
                } else {
                    0.0
                };
                self.downloads
                    .progress(&key, *received, (*total > 0).then_some(*total), fraction);
                if self.download_save_due.is_none() {
                    self.download_save_due =
                        Some(ui::time::now() + std::time::Duration::from_secs(1));
                }
                return;
            }
            "complete" => self.downloads.finished(&key, path.into()),
            "cancelled" => self.downloads.failed(&key, error.clone(), true),
            _ => self.downloads.failed(&key, error.clone(), false),
        }
        self.persist_downloads_now();
        self.download_save_due = None;
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
        // After an extension's keyword, Enter is the extension's: it acts on
        // the words, often sending the page somewhere, as Send words to
        // extension does.
        if let Some(request) = self.extensions.omnibox_enter(&url) {
            if let Err(error) = self.extensions.request(&request) {
                tracing::warn!(%error, "the extension's keyword was not sent");
            }
            self.structure_dirty = true;
            return;
        }
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
            crate::blocker::spawn_filter_loader(self.engine.clone(), None);
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
        row: f32,
    ) -> Option<(PanelKind, Box<dyn Widget>, Vec<String>)> {
        let width = width.max(1.0);
        // A popup the page just opened (a sign-in) comes first: the page
        // waits on it.
        if let Some(popup) = self.page_popups.panel(width, max_height, row) {
            return Some((
                PanelKind::Popup,
                popup,
                anchor_ids(&[URL_INPUT_ID, toolbar::OVERFLOW_ID]),
            ));
        }
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
        if !self.capabilities.asks_for_extension()
            && let Some((content, mut anchors)) = self.extensions.panel(width, max_height)
        {
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
        // A page's popup, or an extension's popup or settings page, takes
        // its own width.
        let width = self.page_popups.width(room).or_else(|| {
            (!self.credentials.open && !self.site.open)
                .then(|| self.extensions.page_width(room))
                .flatten()
        });
        let width = width.unwrap_or_else(|| metrics.panel_width_list.min(room));
        let toolbar_bottom = self
            .find_rect("browser.toolbar")
            .map_or(0.0, |toolbar| toolbar.y1);
        let max_height =
            (window.height() as f32 - toolbar_bottom as f32 - spacing.s3 - pad).max(160.0);
        let (kind, content, anchors) =
            self.anchored_panel_parts(width, max_height, metrics.panel_row_height)?;
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
        // Escape in the address field is the field's: it closes the
        // suggestions or puts the address back, never Find or the load.
        let escape_is_the_address = key.logical_key == WKey::Named(WNamedKey::Escape)
            && (self.url_input_focused() || self.suggestions.is_visible());
        if self.daily.open
            && !escape_is_the_address
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
        if key.logical_key == WKey::Named(WNamedKey::Escape)
            && self.progress < 1.0
            && !escape_is_the_address
        {
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
        // Alt+Left and Alt+Right go back and forward, except on macOS,
        // where Option+Arrow moves the caret by word (Cmd+[ and Cmd+] go).
        let alt_navigation = modifiers.alt_key()
            && !modifiers.control_key()
            && !modifiers.super_key()
            && ui::platform::PlatformConventions::default().platform()
                != ui::platform::Platform::MacOs;
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

    pub(crate) fn focus_control(&mut self, id: &str) -> bool {
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
        // The side panel's edge is no control of the composed tree: it
        // lets go of the keyboard as any of them does.
        if let Some(edge) = self.side_panel_edge.as_deref_mut() {
            edge.set_focused(false);
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
        // Ctrl+Shift+L is not the address's: password managers suggest it
        // for filling a login (an extension's keyboard shortcut).
        let requested = key.state == ElementState::Pressed
            && (modifiers.control_key() || modifiers.super_key())
            && !modifiers.alt_key()
            && !modifiers.shift_key()
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

/// The page zooms by itself, per site: the window's zoom gestures
/// (Ctrl+Plus/Minus/0, Ctrl+scroll, a pinch) step the page's zoom, and the
/// window shows the level it reports.
impl ui::content_zoom::ContentZoom for BrowserWidget {
    fn zoom_content(&mut self, step: ui::content_zoom::ZoomStep) -> Option<f32> {
        use ui::content_zoom::ZoomStep;
        if self.show_settings || self.show_library {
            return None;
        }
        let step = match step {
            ZoomStep::In => 1,
            ZoomStep::Out => -1,
            ZoomStep::Reset => 0,
        };
        self.daily
            .act(crate::daily_controls::Action::Zoom(step), &self.engine);
        self.structure_dirty = true;
        Some(self.daily.zoom_factor())
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

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        self.provide_capabilities(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        self.provide_capabilities_mut(request);
    }

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
        // The page's question stands over everything, the panel too: the
        // dialog's node covers the window out of flow.
        self.page_dialog_node = None;
        if let Some(dialog) = self.page_dialogs.widget_mut() {
            let node = dialog.build_layout(taffy, theme, node_map);
            children.push(node);
            self.page_dialog_node = Some(node);
        }
        self.side_panel_edge_node = self.build_side_panel_edge(taffy, theme, node_map);
        children.extend(self.side_panel_edge_node);
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
        if let (Some(node), Some(dialog)) = (self.page_dialog_node, self.page_dialogs.widget_mut())
        {
            dialog.apply_layout(taffy, node, Point::new(self.rect.x0, self.rect.y0));
        }
        // The suggestions hang under the address field, as wide as it.
        if let Some(r) = self.address_input_rect() {
            self.suggestions.set_anchor(r, self.rect);
        }
        self.place_side_panel_edge();
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
        // Through any cache an ancestor records this into at the identity.
        let coefficients = ui::scroll_copy::on_screen(transform).as_coeffs();
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
        self.paint_side_panel_edge(scene, transform, theme);
        // One hairline under the toolbar row separates it from the page;
        // while the page loads, the loading line takes its place.
        if let Some(rect) = self.find_rect("browser.toolbar") {
            paint_rule(scene, transform, theme, rect, RuleEdge::Bottom, None);
            if let Some(face) = self.loading_face() {
                let height = f64::from(theme.tokens.metrics.chrome_rule_px.max(1.0)) * 2.0;
                face.paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(rect.x0, rect.y1 - height, rect.x1, rect.y1),
                );
            }
        }
    }

    /// The page's damage, and the toolbar row's: what this widget paints
    /// itself (the private tint, the band and loading line) is all in that
    /// row, and its overlays are the window's top layer, which the window
    /// repaints whole.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        let Some(composed) = self.composed.element() else {
            regions.push(self.rect);
            return;
        };
        composed.collect_damage(regions);
        if let Some(toolbar) = self.find_rect("browser.toolbar") {
            regions.push(toolbar);
        }
        regions.extend(self.side_panel_edge_damage());
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
        // Re-anchor under the address field every draw (layout may not have
        // re-run since the suggestions changed).
        if let Some(r) = self.address_input_rect() {
            self.suggestions.set_anchor(r, self.rect);
        }
        self.suggestions
            .draw_overlay(scene, transform, text_renderer, theme, presence);

        self.page_menu
            .menu
            .draw(scene, transform, text_renderer, theme, presence);
        self.back_menu
            .draw(scene, transform, text_renderer, theme, presence);
        if let Some(dialog) = self.page_dialogs.widget_mut() {
            dialog.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
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
            || self
                .page_dialogs
                .widget()
                .is_some_and(Widget::needs_animation_frame)
    }

    /// The page holding a button pressed on it keeps the pointer: the
    /// window hands it the drag's moves and its release, wherever they land.
    fn wants_pointer_capture(&self) -> bool {
        self.side_panel_edge
            .as_ref()
            .is_some_and(|edge| edge.is_dragging())
            || self
                .composed
                .element()
                .is_some_and(Widget::wants_pointer_capture)
    }

    /// The side panel's edge lit or moved since it was drawn needs a frame,
    /// as anything the composed tree, the popover or the dialog changed.
    fn needs_redraw(&self) -> bool {
        let children = self.children();
        children.is_empty()
            || self.side_panel_edge_damage().is_some()
            || children.iter().any(|child| child.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Toggling the settings pane swaps the content widget → rebuild.
        let own = std::mem::take(&mut self.structure_dirty);
        self.composition_dirty |= own;
        let dialog = self
            .page_dialogs
            .widget_mut()
            .is_some_and(Widget::take_structure_dirty);
        own || self.composed.take_structure_dirty() || dialog
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if let Some(composed) = self.composed.element_mut() {
            composed.set_motion_reduced(reduced);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
        }
        if let Some((node, id)) = self.pending_chrome_focus.take() {
            self.focus_control_preserving_selection(id);
            *ctx.focus_target = Some(node);
        }
        if let (Some(node), Some(dialog)) = (self.page_dialog_node, self.page_dialogs.widget_mut())
        {
            // The page waits on its question: the dialog owns the pointer
            // and the keyboard until it is answered.
            dialog.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            self.structure_dirty |= self.page_dialogs.poll(&self.engine);
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
            self.page_menu.settle(&self.engine);
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
        // Past the dialog, popover and menu above, a key reaches the page.
        if self.types_a_password(event) {
            self.hold_sign_in();
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
        // The side panel's edge takes a press on it, the drag that follows
        // and a keyboard step while it has focus.
        if self.side_panel_edge_event(event, ctx) {
            return;
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

        // One of extensions' keyboard shortcuts, pressed while the address
        // has the keyboard: its command runs over the page before the field
        // sees the key, as in Chrome, after the Browser's own keys above. A
        // key the page has is the engine's to look up.
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && self.url_input_focused()
            && let Some(input) = ui::widget::WebView::map_key(&key.logical_key)
        {
            let held = ctx.modifiers.state();
            let modifiers = ui::webview::KeyModifiers {
                ctrl: held.control_key(),
                shift: held.shift_key(),
                alt: held.alt_key(),
                meta: held.super_key(),
            };
            if self.extensions.command_key(&input, modifiers, &self.engine) {
                *ctx.handled = true;
                return;
            }
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
                self.choose_row(&row);
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
            // The suggestions under the address, and the side panel's edge.
            let nodes: Vec<_> = self
                .suggestions
                .accessibility()
                .into_iter()
                .chain(self.side_panel_edge_access())
                .collect();
            return (!nodes.is_empty()).then(|| {
                AccessNode::new(Role::Group)
                    .with_name("Browser")
                    .with_children(nodes)
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
        if self.side_panel_edge_action(target, action) {
            return true;
        }
        if self
            .suggestions
            .perform_accessibility_action(target, action)
        {
            // This queues Browser's existing typed `browser.page/navigate`
            // action — AT-SPI does not get a parallel imperative navigation
            // route.
            if let Some(row) = self.suggestions.take_chosen() {
                self.choose_row(&row);
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
        if let Some(capture) = self.page_dialogs.widget().and_then(Widget::modal_capture) {
            return Some(capture);
        }
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
            .chain(self.page_dialogs.widget())
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
            .chain(self.page_dialogs.widget_mut())
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

/// What a window publishes of its page: the rows it shows, the edits
/// bound to the page and its row, the question the page waits on, and what
/// the search finds of the history, downloads and personal bookmarks.
pub(crate) struct PagePublication {
    pub(crate) rows: Vec<ui::framework::LiveRow>,
    pub(crate) bound: Vec<ui::tilo_resource::ActionDecl<Browser>>,
    page_row: Vec<ui::tilo_resource::ActionDecl<Browser>>,
    question: Option<ui::framework::LiveRow>,
    search_index: crate::search_provider::BrowserSearchIndex,
    personal_bookmarks: Rc<crate::personal_bookmarks::PersonalBookmarkState>,
}

impl PagePublication {
    /// The document edits bound to the row `row`.
    pub(crate) fn row_edits(&self, row: &str) -> Vec<ui::tilo_resource::ActionDecl<Browser>> {
        if row == "browser.page" {
            self.page_row.clone()
        } else {
            Vec::new()
        }
    }

    /// The row a history or download hit opens, beyond the published page
    /// of rows too.
    pub(crate) fn row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if id == crate::page_dialogs::ROW {
            return self.question.clone();
        }
        self.search_index
            .resource(id)
            .or_else(|| crate::personal_bookmarks::resolve(&self.personal_bookmarks, id))
            .map(ui::framework::LiveRow::from_resource)
    }

    /// Every history page, download and personal bookmark as a search entry.
    pub(crate) fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        search_entries(&self.search_index, &self.personal_bookmarks)
    }
}

fn search_entries(
    index: &crate::search_provider::BrowserSearchIndex,
    personal: &crate::personal_bookmarks::PersonalBookmarkState,
) -> Vec<ui::tilo_resource::SearchEntry> {
    let mut entries = index.entries();
    entries.extend(crate::personal_bookmarks::search(personal));
    entries
}

impl BrowserWidget {
    /// The index of history pages and downloads, shared with the opener of
    /// their search hits.
    pub(crate) fn search_index(&self) -> crate::search_provider::BrowserSearchIndex {
        self.search_index.clone()
    }

    /// Every history page and download as a search entry.
    #[cfg(test)]
    pub(crate) fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        search_entries(&self.search_index, &self.personal_bookmarks)
    }

    /// What the window publishes of this page as it is now.
    pub(crate) fn publication(&self) -> PagePublication {
        PagePublication {
            rows: self.live_rows(),
            bound: self.page_declarations(),
            page_row: self.page_row_declarations(),
            question: self
                .page_dialogs
                .resource()
                .map(ui::framework::LiveRow::from_resource),
            search_index: self.search_index.clone(),
            personal_bookmarks: Rc::clone(&self.personal_bookmarks),
        }
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
        reason = "ResourceSpec definitions stay together so their action contracts remain discoverable."
    )]
    pub(crate) fn published(&self) -> ResourceSpec {
        use crate::effects::BrowserEffect as E;
        use ui::tilo_resource::Effect as _;
        const MATERIALIZED_LIMIT: usize = 100;
        let mut page = ResourceSpec::new(
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
        let mut pip = E::PictureInPicture.bound();
        if self.active_pip {
            pip.type_mut().label = "Close Picture in Picture".into();
        }
        page.actions.push(pip);
        let web_page = is_supported_web_url(&self.current_uri);
        let mut reader = E::ReaderView.bound();
        if self.reader_view {
            reader.type_mut().label = "Close reader view".into();
        }
        page.actions.push(if web_page {
            reader
        } else {
            reader.unavailable("no_web_page", "Open a web page first")
        });
        page.actions.extend(self.daily.zoom_actions());
        // A page waiting on a permission asks in the page's own actions too:
        // whoever reads the page answers it there, next to the page's other
        // buttons, and never has to find the prompt in the chrome.
        page.actions.extend(self.capabilities.permission_answers());
        let element = String::new();
        for (spec, asking) in [
            (E::ReadPage { frame: None }, &["frame"][..]),
            (
                E::FindPageElement {
                    role: None,
                    name: None,
                    nth: None,
                    frame: None,
                },
                &["role", "name", "nth", "frame"][..],
            ),
            (
                E::ClickPage {
                    element: element.clone(),
                    frame: None,
                },
                &["element", "frame"][..],
            ),
            (
                E::FillPage {
                    element: element.clone(),
                    text: String::new(),
                    frame: None,
                },
                &["element", "text", "frame"][..],
            ),
            (
                E::SelectPage {
                    element: element.clone(),
                    values: Vec::new(),
                    frame: None,
                },
                &["element", "values", "frame"][..],
            ),
            (
                E::FillPageFiles {
                    element: element.clone(),
                    paths: Vec::new(),
                    frame: None,
                },
                &["element", "paths", "frame"][..],
            ),
            (
                E::PressPage {
                    key: String::new(),
                    element: None,
                    frame: None,
                },
                &["key", "element", "frame"][..],
            ),
            (
                E::FillPageSecret {
                    element: element.clone(),
                    text: String::new(),
                    frame: None,
                },
                &["element", "text", "frame"][..],
            ),
            (
                E::SubmitPage {
                    element,
                    cross_origin: None,
                    frame: None,
                },
                &["element", "cross_origin", "frame"][..],
            ),
            (E::ReadPageChanges { frame: None }, &["frame"][..]),
            (
                E::ReadPageElementAt {
                    x: 0.0,
                    y: 0.0,
                    frame: None,
                },
                &["x", "y", "frame"][..],
            ),
            (
                E::HoverPage {
                    element: None,
                    x: None,
                    y: None,
                },
                &["element", "x", "y"][..],
            ),
            (
                E::DragPage {
                    from: None,
                    from_x: None,
                    from_y: None,
                    to: None,
                    to_x: None,
                    to_y: None,
                    steps: None,
                },
                &["from", "from_x", "from_y", "to", "to_x", "to_y", "steps"][..],
            ),
            (
                E::PointerPath {
                    element: None,
                    points: Vec::new(),
                },
                &["element", "points"][..],
            ),
            (
                E::WaitPageSettled {
                    quiet_ms: None,
                    timeout_ms: None,
                    frame: None,
                },
                &["quiet_ms", "timeout_ms", "frame"][..],
            ),
            (E::ReadPageConsole { since: None }, &["since"][..]),
            (E::ReadPageNetwork { since: None }, &["since"][..]),
            (
                E::CapturePage {
                    element: None,
                    max_width: None,
                    frame: None,
                },
                &["element", "max_width", "frame"][..],
            ),
            (
                E::PointerPage {
                    steps: Vec::new(),
                    button: None,
                },
                &["steps", "button"][..],
            ),
            (
                E::ScrollPage {
                    element: None,
                    to: Some(String::new()),
                    by: None,
                    into_view: Some(String::new()),
                    frame: None,
                },
                &["element", "to", "by", "into_view", "frame"][..],
            ),
        ] {
            let observes = matches!(
                spec.clone().operation(),
                BrowserOperation::Page(request) if request.only_observes()
            );
            let mut spec = spec.bound_asking(asking);
            // Reading a page that holds the person's sign-ins takes the
            // authority to operate it. Without them it takes only what any
            // visitor has: what only looks at the page is a read. Acting
            // on the page reaches the web either way.
            if observes && !self.page_holds_sign_in() {
                spec.type_mut().contract.kind = ui::tilo_actions::ActionKind::Read;
            }
            page.actions.push(if web_page {
                spec
            } else {
                spec.unavailable("no_web_page", "Open a web page first")
            });
        }

        let mut session = ResourceSpec::new(
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
            pause.type_mut().label = "Resume history in this window".into();
        }
        session.actions.push(pause);

        // The Library's views (show history, downloads) are the Library
        // row's: a view on these rows would be their disclosure in the
        // action directory, a private one an agent is never shown, and with
        // it every action here (clear history, start a download).
        let mut history = ResourceSpec::new("browser.history", "browser.history", "History");
        let clear = E::ClearHistory.bound();
        history.actions.push(if self.history.is_empty() {
            clear.unavailable("already_empty", "Browsing history is already empty")
        } else {
            clear
        });
        // Newest first: the pages a person just saw are the ones the
        // Library lists and opens through these rows. The record keeps them
        // by address, so its own order is the alphabet's.
        let mut newest: Vec<&crate::history::HistoryEntry> =
            self.history.entries().iter().collect();
        newest.sort_by_key(|entry| std::cmp::Reverse(entry.last_visit));
        for entry in newest.into_iter().take(MATERIALIZED_LIMIT) {
            let title = if entry.title.trim().is_empty() {
                entry.url.as_str()
            } else {
                entry.title.as_str()
            };
            let mut row = ResourceSpec::new(
                history_resource_id(&entry.url),
                "browser.history-entry",
                title,
            )
            .with_action(
                E::Open {
                    url: entry.url.clone(),
                }
                .bound(),
            );
            // Forgetting a visit is the person's own record: only a window
            // that keeps one offers the row its removal.
            if self.person_history.is_some() {
                row = row.with_action(
                    E::ForgetVisit {
                        url: entry.url.clone(),
                    }
                    .bound(),
                );
            }
            history.children.push(row);
        }

        let mut bookmarks =
            ResourceSpec::new("browser.bookmarks", "browser.bookmarks", "Bookmarks");
        bookmarks.actions.push(E::ShowBookmarks.bound());
        // Each bookmark is an item of the document; this window adds opening
        // its page to it.
        let bookmark_rows: Vec<ResourceSpec> = self
            .bookmarks
            .entries()
            .iter()
            .take(MATERIALIZED_LIMIT)
            .map(|entry| {
                ResourceSpec::new(
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

        let mut downloads =
            ResourceSpec::new("browser.downloads", "browser.downloads", "Downloads");
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
            // No Show view on the row: a search hit shows the download
            // through its own resource, and a view here would be the row's
            // disclosure, a private one that keeps its actions from agents.
            let mut child = ResourceSpec::new(
                entry.resource_id(),
                "browser.download",
                entry.display_name(),
            );
            match entry.status {
                DownloadStatus::AwaitingDestination => {
                    // The page is offering the file: keep it or discard it.
                    // Both ride the download's own id, so an agent reading a
                    // download's row answers the same question the person's
                    // prompt does.
                    child = child
                        .with_action(E::SaveDownload { id: id.clone() }.bound())
                        .with_action(E::DiscardDownload { id }.bound());
                }
                DownloadStatus::Downloading => {
                    child = child.with_action(E::CancelDownload { id }.bound());
                }
                DownloadStatus::Failed | DownloadStatus::Cancelled => {
                    child = child.with_action(E::RetryDownload { id }.bound());
                }
                DownloadStatus::Completed => {
                    child = child
                        .with_action(E::ReadDownload { id: id.clone() }.bound())
                        .with_action(E::OpenDownload { id: id.clone() }.bound())
                        .with_action(E::RevealDownload { id }.bound());
                }
            }
            downloads.children.push(child);
        }

        let mut extensions =
            ResourceSpec::new("browser.extensions", "browser.extensions", "Extensions");
        extensions.actions.push(E::ShowExtensions.bound());
        extensions.actions.extend(self.extensions.add_action());
        // Each installed extension, as the window last read the profile,
        // with what its Library row offers: no disk read per publication.
        extensions.children.extend(self.extensions.resources());

        // Library is a first-class presentation resource rather than an
        // unaddressable collection of tabs.  This lets keyboard users,
        // assistive technology, global action search, and agents all use the
        // same section operations as the visible surface.
        let mut library = ResourceSpec::new("browser.library", "browser.library", "Library");
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

        let mut settings = ResourceSpec::new("browser.settings", "browser.settings", "Settings");
        settings.actions.push(E::RequestSiteDataClear.bound());

        let mut browser = ResourceSpec::new(
            crate::resource::Browser::KIND,
            crate::resource::Browser::KIND,
            "Browser",
        );
        browser.actions = E::specs();
        browser.children = vec![
            page, session, history, bookmarks, downloads, extensions, library, settings,
        ];
        browser.children.extend(bookmark_rows);
        browser.children.extend(self.page_dialogs.resource());
        browser.children.extend(self.capabilities.resource());
        for selection in &self.service_selections {
            browser.children.push(selection.resource());
        }
        browser.children.push(
            ResourceSpec::new(
                "browser.service-sessions",
                "browser.service-sessions",
                "Accounts",
            )
            .with_action({
                let list = E::ListServiceSessions.bound();
                if self.browsing_mode.holds_account_custody() {
                    list
                } else {
                    list.unavailable(
                        "no_account_custody",
                        crate::browsing_mode::NO_ACCOUNT_CUSTODY,
                    )
                }
            }),
        );
        if self.service_view.is_some()
            || self.service_pending.is_some()
            || self.service_forget.is_some()
            || self.service_observation.is_some()
        {
            browser.children.push(
                ResourceSpec::new(
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
        self.poll_invocation_cancelling(invocation, false)
    }

    /// [`Self::poll_invocation`], stopping the operation when its caller no
    /// longer waits for it.
    ///
    /// Page actions declare cooperative cancellation; a request nobody waits
    /// for any more must not hold the tab's one page slot until its deadline.
    /// Stopping it tells the engine so the next action can run, and answers
    /// that what became of the stopped one is unknown rather than claiming it
    /// happened or that it never did.
    pub(crate) fn poll_invocation_cancelling(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        if let Some(stopped) =
            self.answer_a_cancelled_page_operation(invocation, cancellation_requested)
        {
            return Some(stopped);
        }
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
        if let Some(index) = self
            .replaced_navigations
            .iter()
            .position(|update| update.invocation() == invocation)
        {
            return Some(self.replaced_navigations.swap_remove(index));
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
            return Some(action_failed(
                invocation,
                "browser.picture_in_picture_timed_out",
                "Picture in Picture did not receive a media response in time.",
                Some(serde_json::json!({
                    "timeout_seconds": PIP_OPERATION_TIMEOUT.as_secs(),
                })),
            ));
        }
        if self.resource_navigation_invocation == Some(invocation)
            && self
                .resource_navigation_deadline
                .is_some_and(|deadline| ui::time::now() >= deadline)
        {
            self.resource_navigation_invocation = None;
            self.resource_navigation_deadline = None;
            self.cancel_owned_navigation();
            return Some(action_failed(
                invocation,
                "browser.navigation_timed_out",
                "The page did not finish loading in time.",
                Some(serde_json::json!({
                    "timeout_seconds": NAVIGATION_TIMEOUT.as_secs(),
                })),
            ));
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

    /// Whether an effect reply still waits on `invocation`, the one that
    /// would read that operation's outcome.
    fn reply_outstanding(&self, invocation: ui::tilo_actions::InvocationId) -> bool {
        self.pending_replies
            .iter()
            .any(|(waiting, _)| *waiting == invocation)
    }

    /// Let go of a settled page update when no effect reply waits on it any
    /// more: nothing would ever read it, and holding the tab's one
    /// page-operation slot for it refuses every later action forever. An
    /// operation still running is left to settle at its own deadline.
    fn abandon_unwatched_page_result(&mut self) {
        if self
            .page_operation_result
            .as_ref()
            .is_some_and(|update| !self.reply_outstanding(update.invocation()))
        {
            self.page_operation_result = None;
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
                let first = (!self.explicit_navigation_requested)
                    .then(|| WebViewCommand::Navigate(self.initial_url.clone()));
                if self.settings.adblock_enabled {
                    // Scriptlet-style blocking (YouTube ad-skip etc.) the
                    // declarative content filter can't express.
                    self.install_blocking_scripts();
                    // The first page loads once the cached rules are in.
                    crate::blocker::spawn_filter_loader(self.engine.clone(), first);
                } else {
                    self.set_adblock(false);
                    if let Some(first) = first {
                        self.engine.send(first);
                    }
                }
            }
            self.bootstrap_done = true;
        }

        self.structure_dirty |= self.extensions.sync(&self.engine, &self.current_uri);
        self.structure_dirty |= self.page_popups.sync(&self.engine);
        self.structure_dirty |= self.page_dialogs.sync(&self.engine);
        // Whichever engine shows the page now copies to the system clipboard.
        self.engine.share_clipboard();
        self.structure_dirty |= self.page_dialogs.poll(&self.engine);
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
        self.apply_engine_events();
        self.apply_panel_events();
        self.sync_window_toolbar();
        self.structure_dirty |= self.run_pending_commands();
        self.apply_review_stage();
        self.drive_page_operation();
        self.structure_dirty |= self.capabilities.ask(self.extensions.take_questions());
        let downloads_changed = self.capabilities.downloads_changed();
        self.structure_dirty |= downloads_changed;
        // The download that kept the tab from closing finished or was
        // cancelled: the tab can close now, and the notice saying it cannot
        // goes.
        if self.close_blocked && downloads_changed && !self.capabilities.exit_requires_choice() {
            self.close_blocked = false;
        }
        self.sync_search_and_suggestions();
        // Suggestions for what was just typed in the bar go out this frame.
        self.declare_window_toolbar();

        if let Some(c) = self.composed.element_mut() {
            c.sync(state, path);
        }
        if let Some(dialog) = self.page_dialogs.widget_mut() {
            dialog.sync(state, path);
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
        match &operation {
            BrowserOperation::ReadDownload { id } => {
                let value = self.read_download(id)?;
                return Ok(ui::framework::Effected::value(value));
            }
            BrowserOperation::AnswerDownload { id, allow } => {
                let value = self.answer_download(id, *allow)?;
                return Ok(ui::framework::Effected::value(value));
            }
            _ => {}
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
            // Refused at once (the page waits on its question), nothing is
            // to follow the reply.
            self.reader_request = self.page_operation.is_some().then_some(enabled);
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
            let is_zoom = matches!(&operation, BrowserOperation::Zoom { .. });
            let output = match &operation {
                BrowserOperation::ForgetVisit { .. } => {
                    local_output("Browsing history entry removed.")
                }
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
                BrowserOperation::OpenInNewTab { .. } => local_output(
                    "Asked the desktop for a new Browser tab beside this one. The tab is another \
                     Browser instance with its own peer id and its own page: this window keeps this \
                     page and its refs, and the new tab's page has to be addressed through that \
                     instance (tilo_list_apps shows it once it maps; its page actions live there, \
                     never here).",
                ),
                _ => ui::tilo_actions::ActionOutput::default(),
            };
            let answers_a_dialog = matches!(operation, BrowserOperation::AnswerPageDialog { .. });
            let answers_permission = matches!(operation, BrowserOperation::AnswerPermission { .. });
            self.perform_browser_operation(operation)?;
            let output = if answers_a_dialog {
                // Written when the question is answered, so the answer says
                // what it did and what to do next.
                self.page_dialog_answer
                    .take()
                    .map_or_else(ui::tilo_actions::ActionOutput::default, local_output)
            } else if answers_permission {
                self.permission_answer
                    .take()
                    .map_or_else(ui::tilo_actions::ActionOutput::default, local_output)
            } else if is_history_pause_toggle {
                local_output(if self.history_paused() {
                    "History paused in this window."
                } else {
                    "History resumed in this window."
                })
            } else if is_zoom {
                local_output(format!("The page is at {}%", self.daily.zoom_percent()))
            } else {
                output
            };
            return Ok(ui::framework::Effected::Done(output));
        }
        // A navigation while another still loads replaces it, as in every
        // browser: the engine stops the old one when the new one arrives, and
        // the old one's outcome no longer matches a request of ours.
        if let Some(previous) = self.resource_navigation_invocation.take() {
            self.resource_navigation_request = None;
            self.resource_navigation_deadline = None;
            self.replaced_navigations.push(action_failed(
                previous,
                "browser.navigation_replaced",
                "Another navigation replaced this one.",
                None,
            ));
        }
        self.replaced_navigations
            .extend(self.resource_navigation_result.take());
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
            LibrarySection::History => (
                crate::actions::HISTORY_SCOPE,
                "browser-library-section-history",
            ),
            LibrarySection::Bookmarks => (
                crate::actions::BOOKMARKS_SCOPE,
                "browser-library-section-bookmarks",
            ),
            LibrarySection::Downloads => (
                crate::actions::DOWNLOADS_SCOPE,
                "browser-library-section-downloads",
            ),
            LibrarySection::Extensions => {
                ("browser.extensions", "browser-library-section-extensions")
            }
        };
        Some(ui::search::ScopeDecl::new(
            id,
            crate::resource::CATALOG.t(label),
        ))
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
