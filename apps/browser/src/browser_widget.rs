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
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::widget::{AppModel, ModelPath};
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
use ui::webview::{
    MediaElementInfo, NavigationDirection, PageFailure, PageKey, PageReply, PageRequest,
    SharedEngine, WebViewCommand, WebViewEvent,
};
use ui::widget::toast::{ToastCard, ToastKind, paint_card};
use ui::widget::{
    AppKind, AppWidget, Banner, Button, Column, Container, ContextMenu, Continuable, ControlSize,
    EmptyState, FloatingSurfaceProvider, IconDescriptor, MenuItem, ProgressBar, WebView, Widget,
};

use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, history_resource_id,
};
use crate::address_overlay::AddressSuggestions;
use crate::browsing_mode::BrowsingMode;
use crate::downloads::{DownloadStatus, Downloads};
use crate::history::History;
use crate::library::Bookmarks;
use crate::settings::{Settings, SettingsActionBindings, SettingsPane, SettingsPresentation};

mod library;
mod toolbar;

const PIP_ID: &str = "browser.media-pip";
const PIP_ELEMENT_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);
/// How many address-bar suggestions to show.
const SUGGESTION_LIMIT: usize = 8;
/// e2e id of the address input (also used internally to detect whether
/// the user is editing it).
const URL_INPUT_ID: &str = "browser.url_input";
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
/// Whether a window `width` (in 1× pixels) gets the compact toolbar: below the
/// system's compact breakpoint it keeps navigation, the address field and one
/// overflow menu, and the commands that no longer fit join the menu.
fn is_compact_chrome(width: f64, breakpoint: f32) -> bool {
    width < f64::from(breakpoint)
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
    Some(vello::peniko::ImageData {
        data: vello::peniko::Blob::new(std::sync::Arc::new(decoded.rgba)),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width: expected_width,
        height: expected_height,
    })
}

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
    started: ui::time::Instant,
    /// Screenshot/debug guidance is deliberately steady: headless rendering
    /// can be queued behind a busy renderer, and must not race the ordinary
    /// short-lived live toast away before it is inspected.
    persistent: bool,
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
            started: ui::time::now()
                .checked_sub(PIP_FEEDBACK_FADE)
                .unwrap_or_else(ui::time::Instant::now),
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

#[derive(Clone, Debug)]
enum BrowserOperation {
    Back,
    Forward,
    Reload,
    Stop,
    Navigate {
        url: String,
    },
    ShowLibrary {
        section: LibrarySection,
    },
    ShowSettings,
    /// Open the passwords panel, or the address autofill panel.
    ShowAutofill {
        addresses: bool,
    },
    NewPrivateWindow,
    ShowPage,
    ClearHistory,
    RequestWebsiteDataClear,
    TogglePictureInPicture,
    ToggleHistoryPause,
    StartDownload {
        url: String,
    },
    CancelDownload {
        id: String,
    },
    RetryDownload {
        id: String,
    },
    OpenDownload {
        id: String,
    },
    RevealDownload {
        id: String,
    },
    Page(PageRequest),
}

fn page_operation_payload(request: &PageRequest) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert("operation".into(), "page".into());
    payload.insert("action".into(), request.action().into());
    if let Some(element) = request.element() {
        payload.insert("element".into(), element.into());
    }
    let argument = match request {
        PageRequest::Fill { text, .. } => Some(("text", text.as_str().into())),
        PageRequest::Select { values, .. } => Some(("values", values.clone().into())),
        PageRequest::Press { key, .. } => Some(("key", key.chord().into())),
        PageRequest::Outline | PageRequest::Click { .. } | PageRequest::Submit { .. } => None,
    };
    if let Some((name, value)) = argument {
        payload.insert(name.into(), value);
    }
    payload.into()
}

fn decode_page_operation(value: &serde_json::Value) -> Result<PageRequest, String> {
    let element = || required_string(value, "element");
    let request = match value.get("action").and_then(serde_json::Value::as_str) {
        Some("outline") => PageRequest::Outline,
        Some("click") => PageRequest::Click {
            element: element()?,
        },
        Some("fill") => PageRequest::Fill {
            element: element()?,
            text: value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or("page fill is missing `text`")?
                .to_string(),
        },
        Some("select") => PageRequest::Select {
            element: element()?,
            values: value
                .get("values")
                .and_then(serde_json::Value::as_array)
                .ok_or("page select is missing `values`")?
                .iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect::<Option<_>>()
                .ok_or("page select `values` must be strings")?,
        },
        Some("press") => PageRequest::Press {
            element: value
                .get("element")
                .and_then(serde_json::Value::as_str)
                .filter(|element| !element.is_empty())
                .map(str::to_string),
            key: required_string(value, "key").and_then(|chord| {
                PageKey::parse(&chord).ok_or_else(|| {
                    format!(
                        "`{chord}` is not a key such as Enter, Tab, Escape, ArrowDown or Control+a"
                    )
                })
            })?,
        },
        Some("submit") => PageRequest::Submit {
            element: element()?,
        },
        _ => return Err("unknown page action".into()),
    };
    if let Some(element) = request.element()
        && !ui::webview::page::is_element_ref(element)
    {
        return Err(format!(
            "`{element}` is not an element ref; read the page and use a ref such as d1e5"
        ));
    }
    if !request.valid() {
        return Err("page request exceeds its limits".into());
    }
    Ok(request)
}

fn browser_operation_payload(operation: &BrowserOperation) -> serde_json::Value {
    match operation {
        BrowserOperation::Back => serde_json::json!({ "operation": "back" }),
        BrowserOperation::Forward => serde_json::json!({ "operation": "forward" }),
        BrowserOperation::Reload => serde_json::json!({ "operation": "reload" }),
        BrowserOperation::Stop => serde_json::json!({ "operation": "stop" }),
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
        BrowserOperation::ShowAutofill { addresses } => serde_json::json!({
            "operation": "show-autofill",
            "panel": if *addresses { "addresses" } else { "passwords" }
        }),
        BrowserOperation::NewPrivateWindow => {
            serde_json::json!({ "operation": "new-private-window" })
        }
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
        BrowserOperation::Page(request) => page_operation_payload(request),
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
        Some("stop") => Ok(BrowserOperation::Stop),
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
        Some("show-autofill") => match value.get("panel").and_then(serde_json::Value::as_str) {
            Some("passwords") => Ok(BrowserOperation::ShowAutofill { addresses: false }),
            Some("addresses") => Ok(BrowserOperation::ShowAutofill { addresses: true }),
            _ => Err("browser autofill operation has an unknown panel".into()),
        },
        Some("new-private-window") => Ok(BrowserOperation::NewPrivateWindow),
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
        Some("page") => decode_page_operation(&value).map(BrowserOperation::Page),
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

/// Window-level navigation Views/Effects for [`crate::resource::Browser::local_operations`].
///
/// Hosted surfaces empty [`PublishesResources::resources`] so these must ride
/// `tilo_resource::publish` — otherwise the palette loses back / forward /
/// reload / stop / open-address when the widget tree is no longer the door.
pub(crate) fn window_navigation_operations() -> Vec<ActionSpec> {
    let mut ops = Vec::with_capacity(5);
    for (id, label, operation) in [
        ("back", "Go back", BrowserOperation::Back),
        ("forward", "Go forward", BrowserOperation::Forward),
        ("reload", "Reload", BrowserOperation::Reload),
        ("stop", "Stop loading", BrowserOperation::Stop),
    ] {
        let payload = browser_operation_payload(&operation);
        let Ok(spec) = ActionSpec::operation(
            id,
            label,
            OperationKind::View,
            BROWSER_OPERATION_TYPE,
            &payload,
        ) else {
            continue;
        };
        ops.push(if id == "stop" {
            spec
        } else {
            spec.with_contract(navigation_contract())
        });
    }
    ops.push(
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
    ops
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

/// Open the bookmarks library. Published on the bookmarks child so the
/// palette and a hosted browser share one view action.
pub(crate) fn bookmark_library_show() -> Option<ActionSpec> {
    ActionSpec::operation(
        "show",
        "Show bookmarks",
        OperationKind::View,
        BROWSER_OPERATION_TYPE,
        &browser_operation_payload(&BrowserOperation::ShowLibrary {
            section: LibrarySection::Bookmarks,
        }),
    )
    .ok()
}

/// Settings door published with the browser document. Homepage, search
/// engine, ad blocking, and clear-on-exit are document actions; show and
/// site-data clear stay effects on the same resource.
pub(crate) fn settings_resource() -> Resource {
    let mut settings = Resource::new("browser.settings", "browser.settings", "Settings");
    if let Ok(spec) = ActionSpec::operation(
        "show",
        "Show browser settings",
        OperationKind::View,
        BROWSER_OPERATION_TYPE,
        &browser_operation_payload(&BrowserOperation::ShowSettings),
    ) {
        settings.actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::operation(
        "request-site-data-clear",
        "Request local site-data clear",
        OperationKind::Effect,
        BROWSER_OPERATION_TYPE,
        &browser_operation_payload(&BrowserOperation::RequestWebsiteDataClear),
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
        (
            "open-passwords",
            "Show saved passwords",
            BrowserOperation::ShowAutofill { addresses: false },
        ),
        (
            "open-addresses",
            "Show address autofill",
            BrowserOperation::ShowAutofill { addresses: true },
        ),
    ] {
        if let Ok(spec) = ActionSpec::operation(
            id,
            label,
            OperationKind::View,
            BROWSER_OPERATION_TYPE,
            &browser_operation_payload(&operation),
        ) {
            settings
                .actions
                .push(spec.with_contract(local_effect_contract(&[
                    "device-local",
                    "presentation",
                    "sensitive-data",
                ])));
        }
    }
    if let Ok(spec) = ActionSpec::operation(
        "new-private-window",
        "Open a new private window",
        OperationKind::Effect,
        BROWSER_OPERATION_TYPE,
        &browser_operation_payload(&BrowserOperation::NewPrivateWindow),
    ) {
        settings
            .actions
            .push(spec.with_contract(local_effect_contract(&[
                "device-local",
                "window",
                "private-browsing",
            ])));
    }
    for (id, label, adblock) in [
        ("set-adblock", "Set ad and tracker blocking", true),
        ("set-clear-on-exit", "Set clear data on exit", false),
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
                    let action = if adblock {
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
    for (id, label, parameter, homepage) in [
        ("set-homepage", "Set homepage", "url", true),
        (
            "set-search-engine",
            "Set search engine template",
            "template",
            false,
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
                    let action = if homepage {
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
    settings
}

fn local_background_contract(tags: &[&str]) -> ActionContract {
    ActionContract {
        execution: ui::tilo_actions::ExecutionMode::Background { cancellable: false },
        ..local_effect_contract(tags)
    }
}

/// Page content belongs to this profile's signed-in sessions, so reading it
/// takes the same authority as operating it, and neither is offered to a
/// person picking from the palette: people use the page itself.
fn page_contract(read: bool) -> ActionContract {
    ActionContract {
        permission: ui::tilo_actions::PermissionRequirement::Write,
        risk: if read {
            ui::tilo_actions::RiskLevel::None
        } else {
            ui::tilo_actions::RiskLevel::Low
        },
        retry: if read {
            RetryPolicy::Idempotent
        } else {
            RetryPolicy::AtMostOnce
        },
        execution: ui::tilo_actions::ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Json {
            schema: serde_json::json!({
                "type": "object",
                "required": ["url", "title", "message", "navigated"],
                "properties": {
                    "url": { "type": "string" },
                    "title": { "type": "string" },
                    "message": { "type": "string" },
                    "navigated": { "type": "boolean" },
                    "outline": { "type": "string" },
                },
            }),
        },
        tags: vec![
            "device-local".into(),
            "page-content".into(),
            "sensitive-data".into(),
            ui::tilo_actions::PROGRAMMATIC_TAG.into(),
        ],
        ..ActionContract::default()
    }
}

fn element_param() -> ActionParam {
    ActionParam::required_owned(
        "element",
        ParamType::Text,
        "Element ref from browser.page/read, such as d1e5",
    )
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
                "{} of {}, {:.0}%",
                ui::format::bytes_si(entry.received_bytes),
                ui::format::bytes_si(total),
                entry.progress * 100.0
            ),
            None => format!("{} downloaded", ui::format::bytes_si(entry.received_bytes)),
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
    browsing_mode: BrowsingMode,
    composed: Option<Box<dyn Widget>>,
    bootstrap_done: bool,
    /// `--shots` mode: no network, no filter loader, deterministic page.
    headless: bool,
    /// Framework dispatcher when hosted; local action host otherwise.
    /// Bookmarks and preferences are [`BrowserAction`]s on this sink.
    actions: crate::resource::ActionSink,
    /// Hosted bookmark and preference edits, applied on the next sync.
    pending_library_edits: ui::LocalState<Vec<BrowserAction>>,
    initial_url: String,
    session_url: String,
    session_first_commit: bool,
    navigation_deferred: bool,
    /// An address draft belongs to this window. Only an explicit Navigate
    /// resource operation crosses the browser/engine boundary.
    address: ui::LocalState<String>,
    /// Draft value at an explicit navigation; newer edits remain local.
    address_navigation_value: Option<String>,
    daily: crate::daily_controls::DailyControls,
    page_menu: crate::page_menu::PageMenu,
    site: crate::site_controls::SiteControls,
    credentials: crate::credentials::Credentials,
    site_panel: Option<Box<dyn Widget>>,
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
    /// `1.0` when idle; anything below shows the progress bar.
    progress: f64,
    title: String,
    favicon: Option<vello::peniko::ImageData>,
    audio_muted: bool,
    media_playing: bool,
    media_document_generation: Option<u64>,
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
    capabilities: crate::capabilities::Capabilities,
    startup_extensions: Vec<std::path::PathBuf>,
    extension_notice: Option<String>,
    extension_feedback: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    settings_presentation: SettingsPresentation,
    /// History pausing is an in-memory, device-local session boundary. It is
    /// never serialized to the collaborative document or the profile.
    history_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    observed_history_paused: bool,
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
    download_save_due: Option<ui::time::Instant>,
    /// Retained full-corpus index. Provider handles are cheap and unchanged
    /// frames compare only the durable corpus revision tokens.
    search_index: crate::search_provider::BrowserSearchIndex,
    projected_heads: Option<Vec<automerge::ChangeHash>>,
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
    resource_navigation_deadline: Option<ui::time::Instant>,
    resource_pip_invocation: Option<ui::tilo_actions::InvocationId>,
    resource_pip_request: Option<u64>,
    resource_pip_result: Option<ui::tilo_actions::InvocationUpdate>,
    resource_pip_deadline: Option<ui::time::Instant>,
    page_operation: Option<PageOperation>,
    page_operation_result: Option<ui::tilo_actions::InvocationUpdate>,
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
    pip_feedback_rect: Rect,
    pip_float_page_rect: Rect,
    pip_select_area_rect: Rect,
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
    /// Resource actions that menu rows and row actions queued; drained in
    /// `sync` (see `toolbar.rs`).
    pending_commands: ui::LocalState<Vec<toolbar::ResourceCommand>>,
    motion_reduced: bool,
    pending_controls: std::collections::VecDeque<ControlInvocation>,
}

/// A library remove queued before the document is in hand still tombstones
/// the tags the document actually holds.
fn bookmark_remove_with_observed_tags(doc: &AutoCommit, action: BrowserAction) -> BrowserAction {
    match action {
        BrowserAction::RemoveBookmark { url, observed_tags } if observed_tags.is_empty() => {
            let observed_tags = crate::actions::observed_bookmark_tags(doc, &url);
            BrowserAction::RemoveBookmark { url, observed_tags }
        }
        other => other,
    }
}

impl BrowserWidget {
    /// Live-mode widget with the default engine (WPE `WebKit` where the
    /// backend is available, the deterministic test engine otherwise).
    pub fn new() -> Self {
        Self::new_with_mode(BrowsingMode::Normal)
    }

    pub fn new_with_mode(mode: BrowsingMode) -> Self {
        Self::with_engine_mode(default_engine(mode), false, mode)
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
        self.session_url.clone_from(&uri);
        self.initial_url = uri;
        self
    }

    pub(crate) fn prepare_session_uri(&mut self, uri: String, deferred: bool) {
        self.initial_url.clone_from(&uri);
        self.session_url.clone_from(&uri);
        self.address.set(uri);
        self.current_uri.clear();
        self.session_first_commit = false;
        self.navigation_deferred = deferred;
        self.bootstrap_done = false;
    }
    pub(crate) fn activate_session_uri(&mut self) {
        if self.navigation_deferred {
            self.navigation_deferred = false;
            self.bootstrap_done = false;
        }
    }
    pub(crate) fn persisted_url(&self) -> &str {
        &self.session_url
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
    pub(crate) fn content_fullscreen(&self) -> bool {
        self.content_fullscreen
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

    /// Widget over an explicit engine — used by tests.
    pub(crate) fn is_headless(&self) -> bool {
        self.headless
    }
    pub(crate) fn deactivate_credentials(&mut self) {
        let was_open = self.credentials.open;
        let requests = self.credentials.close();
        crate::credentials::Credentials::send(&self.engine, requests);
        self.structure_dirty |= was_open;
    }

    pub(crate) fn browsing_mode(&self) -> BrowsingMode {
        self.browsing_mode
    }
    pub(crate) fn request_private_window(&self) {
        self.private_window_request.set(true);
    }
    pub(crate) fn tab_url(&self) -> &str {
        if self.navigation_deferred || self.current_uri.is_empty() {
            &self.initial_url
        } else {
            &self.current_uri
        }
    }
    pub(crate) fn tab_title(&self) -> String {
        if self.navigation_deferred {
            return self.initial_url.clone();
        }
        let title = if !self.title.is_empty() {
            &self.title
        } else if self.tab_url() == "about:blank" {
            "New tab"
        } else {
            self.tab_url()
        };
        title.to_owned()
    }
    pub(crate) fn tab_visual(&self) -> crate::browser_tab_strip::TabVisual {
        crate::browser_tab_strip::TabVisual {
            favicon: self.favicon.clone(),
            loading: self.progress < 1.0,
            muted: self.audio_muted,
            media_playing: self.media_playing,
        }
    }
    pub(crate) fn tab_muted(&self) -> bool {
        self.audio_muted
    }
    pub(crate) fn set_tab_muted(&mut self, muted: bool) {
        self.audio_muted = muted;
        self.engine.send(WebViewCommand::SetAudioMuted(muted));
        self.structure_dirty = true;
    }
    pub(crate) fn set_tab_identity(&mut self, id: u64) {
        self.pip_id = format!("{PIP_ID}.{id}");
    }
    pub(crate) fn share_history_pause(&mut self, other: &Self) {
        self.history_paused = other.history_paused.clone();
    }
    pub(crate) fn history_paused(&self) -> bool {
        self.history_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn focus_tab_address_with_caret(&mut self, buffered: bool) -> bool {
        let focused = self.focus_address_input();
        if buffered {
            fn caret(widget: &mut dyn Widget) {
                if widget.test_id() == Some(URL_INPUT_ID)
                    && let Some((_, text)) = widget.text_review()
                {
                    ui::undo::set_state(widget, &text.len().to_string());
                }
                for child in widget.children_mut() {
                    caret(child);
                }
            }
            if let Some(composed) = self.composed.as_mut() {
                caret(composed.as_mut());
            }
        }
        focused
    }
    pub(crate) fn buffer_tab_address(&mut self, key: &WKey, replace: bool) -> bool {
        match key {
            WKey::Character(text) => {
                if replace {
                    self.address.set(String::new());
                }
                self.address.update(|address| address.push_str(text));
                true
            }
            WKey::Named(WNamedKey::Backspace) => {
                self.address.update(|address| {
                    address.pop();
                });
                true
            }
            WKey::Named(WNamedKey::Enter) => {
                self.navigate_to_input();
                true
            }
            _ => false,
        }
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
    pub(crate) fn inherit_workspace_state(&mut self, other: &Self) {
        self.window_scale = other.window_scale;
        self.capabilities.share_downloads(&other.capabilities);
        self.site.share_runtime_state(&other.site);
    }
    pub(crate) fn refresh_shared_history(&mut self) {
        if !self.headless && self.browsing_mode.persists_personal_state() {
            self.history = History::load();
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
        // Persisted state; `load()` returns defaults/empty when absent, so
        // `--shots` runs are unaffected by (and don't write) real files.
        let persistent = !headless && browsing_mode.persists_personal_state();
        let settings = if persistent {
            Settings::load()
        } else {
            Settings::default()
        };
        let history = if persistent {
            History::load()
        } else {
            History::default()
        };
        let bookmarks = if persistent {
            Bookmarks::load()
        } else {
            Bookmarks::default()
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
            browsing_mode,
            composed: None,
            bootstrap_done: false,
            headless,
            actions: crate::resource::ActionSink::default(),
            pending_library_edits: ui::LocalState::new(Vec::new()),
            address: ui::LocalState::new(initial_url.clone()),
            address_navigation_value: None,
            daily: crate::daily_controls::DailyControls::default(),
            page_menu: crate::page_menu::PageMenu::default(),
            site: crate::site_controls::SiteControls::default(),
            credentials: crate::credentials::Credentials::new(browsing_mode.is_private()),
            site_panel: None,
            pending_chrome_focus: None,
            private_window_request: ui::LocalState::new(false),
            private_window_notice: None,
            window_scale: 1.0,
            native_closed: false,
            close_blocked: false,
            session_url: initial_url.clone(),
            session_first_commit: false,
            navigation_deferred: false,
            initial_url,
            can_go_back: false,
            loading_stopped: false,
            can_go_forward: false,
            progress: if headless { 1.0 } else { 0.0 },
            title: String::new(),
            favicon: None,
            audio_muted: false,
            media_playing: false,
            media_document_generation: None,
            current_uri: String::new(),
            load_error: None,
            address_error_active: false,
            forced_load_error: false,
            forced_loading: false,
            settings,
            capabilities: crate::capabilities::Capabilities::default(),
            startup_extensions: if persistent {
                crate::extensions::approved_paths()
            } else {
                Vec::new()
            },
            extension_notice: None,
            extension_feedback: std::sync::Arc::default(),
            settings_presentation,
            history_paused: std::sync::Arc::default(),
            observed_history_paused: false,
            history_clear_error: None,
            history_save: History::save,
            history,
            bookmarks,
            downloads,
            download_picker: None,
            download_picker_queue: std::collections::VecDeque::new(),
            download_save_due: None,
            search_index,
            projected_heads: None,
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
            page_operation: None,
            page_operation_result: None,
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
            pip_feedback_rect: Rect::ZERO,
            pip_float_page_rect: Rect::ZERO,
            pip_select_area_rect: Rect::ZERO,
            pending_capture_studio: None,
            cursor: Point::ZERO,
            compact_chrome: false,
            compact_breakpoint: ui::tokens::Metrics::BASE.compact_breakpoint,
            text_scale: 1.0,
            pending_commands: ui::LocalState::new(Vec::new()),
            motion_reduced: false,
            pending_controls: std::collections::VecDeque::new(),
        }
    }

    /// Mark this browser as a framework-hosted surface so
    /// [`Widget::resources`] defers the document root to `tilo_resource::publish`.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.actions.set_hosted(hosted);
    }

    /// Attach the framework dispatcher. Hosted bookmark and preference edits
    /// then go through [`Dispatcher::apply_now_with`](ui::framework::Dispatcher::apply_now_with).
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Browser>,
    ) {
        self.actions.attach(dispatcher);
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Submit one bookmark or preference edit. Hosted surfaces with a
    /// dispatcher use `apply_now_with`; standalone uses the local action host.
    pub(crate) fn submit_library_action(&mut self, doc: &mut AutoCommit, action: &BrowserAction) {
        self.actions.dispatch_with_context(
            doc,
            BROWSER_ACTION_TYPE,
            action,
            crate::actions::history_context(action),
        );
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn last_library_action(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
    }

    fn enqueue_library_action(&self, action: BrowserAction) {
        self.pending_library_edits
            .update(|queue| queue.push(action));
    }

    fn flush_library_edits(&mut self, doc: &mut AutoCommit) {
        let waiting = self.pending_library_edits.with(|queue| !queue.is_empty());
        if !waiting {
            return;
        }
        let mut pending = Vec::new();
        let taken = self
            .pending_library_edits
            .try_update(|queue| pending.append(queue));
        if !taken {
            return;
        }
        for action in pending {
            let action = bookmark_remove_with_observed_tags(doc, action);
            self.submit_library_action(doc, &action);
        }
    }

    /// Add the current page, unless it is already bookmarked. Removal stays
    /// on the library row; the toolbar action is the add.
    fn page_bookmark_action(&self) -> Option<BrowserAction> {
        if self.current_uri.trim().is_empty() || self.bookmarks.contains(&self.current_uri) {
            return None;
        }
        Some(BrowserAction::AddBookmark {
            url: self.current_uri.clone(),
            title: self.title.clone(),
            tag: format!("bookmark:{}", uuid::Uuid::new_v4().simple()),
        })
    }

    /// Standalone keeps the published binding. Hosted document edits queue a
    /// [`BrowserAction`] for the action sink instead of a second resource tree.
    fn route_document_button(
        &self,
        button: ui::widget::Button,
        resource: &str,
        action_id: &str,
        hosted_action: Option<BrowserAction>,
    ) -> ui::widget::Button {
        if !self.is_framework_hosted() {
            return self.bind_button(button, resource, action_id);
        }
        let Some(action) = hosted_action else {
            return button;
        };
        let edits = self.pending_library_edits.clone();
        button.on_effect(move || {
            edits.update(|queue| queue.push(action.clone()));
        })
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
        self.compact_chrome = is_compact_chrome(f64::from(width), self.compact_breakpoint);
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

    /// Handle to the live `WebView` content rect (window fractions), for the
    /// zero-copy compositor. Updated every layout.
    pub fn content_rect_handle(&self) -> std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>> {
        self.content_rect.clone()
    }

    /// Perform a browser operation and report inputs that the Browser is not
    /// permitted to hand to the web engine.  In particular, a browser window
    /// is not a file picker: its package declares only the web schemes, and
    /// passing `file:` through would bypass the portal boundary.
    fn set_loading_progress(&mut self, progress: f64) {
        let progress = progress.clamp(0.0, 1.0);
        if (self.progress < 1.0) != (progress < 1.0) {
            self.structure_dirty = true;
        }
        self.progress = progress;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "browser operations share navigation admission and state transitions"
    )]
    fn perform_browser_operation(&mut self, operation: BrowserOperation) -> Result<(), String> {
        if matches!(
            &operation,
            BrowserOperation::Back
                | BrowserOperation::Forward
                | BrowserOperation::Reload
                | BrowserOperation::Navigate { .. }
        ) {
            self.loading_stopped = false;
            self.structure_dirty |= self.daily.navigation_started(&self.engine);
        }
        match operation {
            BrowserOperation::Back => {
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.engine.send(WebViewCommand::GoBack);
            }
            BrowserOperation::Forward => {
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.engine.send(WebViewCommand::GoForward);
            }
            BrowserOperation::Reload => {
                self.clear_address_error();
                self.set_loading_progress(0.0);
                self.engine.send(WebViewCommand::Reload);
            }
            BrowserOperation::Stop => {
                self.loading_stopped = true;
                self.engine.send(WebViewCommand::StopLoading);
                self.set_loading_progress(1.0);
                if let Some(invocation) = self.resource_navigation_invocation.take() {
                    self.resource_navigation_deadline = None;
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
            BrowserOperation::Navigate { url } => {
                let target = self.navigation_target(url.trim())?;
                self.address_navigation_value = Some(self.address.with(Clone::clone));
                self.clear_address_error();
                self.show_library = false;
                self.show_settings = false;
                self.structure_dirty = true;
                self.set_loading_progress(0.0);
                self.engine.send(WebViewCommand::Navigate(target));
            }
            BrowserOperation::ShowLibrary { section } => {
                self.library_section = section;
                self.show_library = true;
                self.show_settings = false;
                self.structure_dirty = true;
            }
            BrowserOperation::ShowAutofill { addresses } => {
                (self.credentials.opener(addresses))()();
                self.structure_dirty = true;
            }
            BrowserOperation::NewPrivateWindow => self.request_private_window(),
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
        }
        Ok(())
    }

    fn page_loading(&self) -> bool {
        self.progress < 1.0 && !self.loading_stopped
    }

    /// Read and operate the page's content without its pixels: an outline
    /// whose interactive elements carry refs, and actions on those refs.
    fn page_content_actions(&self) -> Vec<ActionSpec> {
        fn page_action(
            id: &str,
            label: &str,
            placeholder: &str,
            params: Vec<ActionParam>,
            build: impl Fn(&ActionArgs) -> PageRequest + Send + Sync + 'static,
        ) -> ActionSpec {
            ActionSpec::typed_operation(
                id,
                label,
                placeholder,
                if id == "read" {
                    OperationKind::View
                } else {
                    OperationKind::Effect
                },
                BROWSER_OPERATION_TYPE,
                params,
                move |args: &ActionArgs| {
                    encode_browser_operation(&BrowserOperation::Page(build(args)))
                },
            )
            .with_contract(page_contract(id == "read"))
        }
        let element = |args: &ActionArgs| args.text("element").unwrap_or_default().to_string();
        let specs = vec![
            page_action("read", "Read page content", "", Vec::new(), |_| {
                PageRequest::Outline
            }),
            page_action(
                "click",
                "Click page element",
                "d1e5",
                vec![element_param()],
                move |args| PageRequest::Click {
                    element: element(args),
                },
            ),
            page_action(
                "fill",
                "Fill page field",
                "text",
                vec![
                    element_param(),
                    ActionParam::required_owned(
                        "text",
                        ParamType::Text,
                        "Text that replaces the field's contents",
                    ),
                ],
                move |args| PageRequest::Fill {
                    element: element(args),
                    text: args.text("text").unwrap_or_default().to_string(),
                },
            ),
            page_action(
                "select",
                "Choose page options",
                "option",
                vec![
                    element_param(),
                    ActionParam::required_owned(
                        "values",
                        ParamType::Array,
                        "Option values or visible labels to choose",
                    )
                    .with_schema(serde_json::json!({
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": ui::webview::page::MAX_SELECT_VALUES,
                    })),
                ],
                move |args| PageRequest::Select {
                    element: element(args),
                    values: args
                        .array("values")
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect(),
                },
            ),
            page_action(
                "press",
                "Press key on page",
                "Enter",
                vec![
                    ActionParam::required_owned(
                        "key",
                        ParamType::Text,
                        "Key or chord such as Enter, Tab, Escape, ArrowDown or Control+a",
                    ),
                    ActionParam::optional_owned(
                        "element",
                        ParamType::Text,
                        "Element ref to focus first; otherwise the focused element",
                    ),
                ],
                |args| PageRequest::Press {
                    element: args
                        .text("element")
                        .filter(|element| !element.is_empty())
                        .map(str::to_string),
                    key: {
                        let chord = args.text("key").unwrap_or_default();
                        PageKey::parse(chord).unwrap_or_else(|| PageKey {
                            key: chord.to_string(),
                            modifiers: ui::webview::KeyModifiers::default(),
                        })
                    },
                },
            ),
            page_action(
                "submit",
                "Submit page form",
                "d1e5",
                vec![element_param()],
                move |args| PageRequest::Submit {
                    element: element(args),
                },
            ),
        ];
        if is_supported_web_url(&self.current_uri) {
            specs
        } else {
            specs
                .into_iter()
                .map(|spec| spec.unavailable("no_web_page", "Open a web page first"))
                .collect()
        }
    }

    fn start_page_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        request: PageRequest,
    ) -> Result<(), String> {
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

    /// Clear the in-memory corpus only when its new empty state is durable.
    /// A failed write restores the prior entries so actions/history never
    /// report local data as cleared when it will reappear on restart.
    fn clear_history_durably(&mut self) -> Result<(), String> {
        let previous = self.history.clone();
        self.history.clear();
        if !self.headless
            && self.browsing_mode.persists_personal_state()
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
        walk(&ui::resources::resources(self, &AutoCommit::new()), wanted)
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
            open_passwords: self.unit_binding("browser.settings", "open-passwords"),
            open_addresses: self.unit_binding("browser.settings", "open-addresses"),
            new_private_window: self.unit_binding("browser.settings", "new-private-window"),
            close: self.unit_binding("browser", "show-page"),
            document_edits: Some(self.pending_library_edits.clone()),
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
            self.next_pip_refresh_at = Some(ui::time::now() + PIP_ELEMENT_REFRESH);
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
            .with_gap(theme.tokens.spacing.s0)
            .push(self.build_toolbar(theme));
        let inset = |banner: Banner| -> Box<dyn Widget> {
            Box::new(Container::new().with_child(Box::new(banner)))
        };
        if self.browsing_mode.is_private() {
            column = column.push(Box::new(
                Container::new()
                    .with_id("browser.private-indicator")
                    .with_child(Box::new(Banner::info("Private window").with_message(
                    "Browsing data is removed when this window closes. Files you download remain.",
                ))),
            ));
        }
        if let Some(message) = &self.private_window_notice {
            column = column.push(inset(Banner::info(message)));
        }
        if self.close_blocked {
            column = column.push(inset(
                Banner::warning("This tab has an active download").with_message(
                    "Finish or cancel it in Downloads in the library, then close the tab.",
                ),
            ));
        }
        if self.daily.open && !self.show_settings && !self.show_library {
            column = column.push(self.daily.bar());
        }
        if let Some(prompt) = self.capabilities.prompt(&self.engine) {
            column = column.push(prompt);
        }
        if self.history_paused() {
            column = column
                .push(inset(Banner::info("History paused").with_message(
                    "Pages in this window are not saved to browsing history.",
                )));
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
            Box::new(
                WebView::new(self.engine.clone())
                    .with_scale_factor(self.window_scale)
                    .with_test_id("browser.surface"),
            )
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
                    .with_gap(theme.tokens.spacing.s0)
                    .with_fill_remaining_height()
                    .push(Box::new(
                        ProgressBar::indeterminate().with_label("Page loading"),
                    ))
                    .push(content),
            ) as Box<dyn Widget>
        } else {
            content
        };
        column = column.push(content);
        Box::new(column)
    }

    fn sync_bookmarks(&mut self, state: &AutoCommit, _root: &ObjId) {
        let Some(incoming) = crate::actions::bookmarks_from_doc(state) else {
            return;
        };
        if incoming != self.bookmarks {
            self.bookmarks = incoming;
            self.search_index.invalidate();
            if !self.headless && self.browsing_mode.persists_personal_state() {
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
        use ui::doc::{read_bool, read_text};

        let mut next = self.settings.clone();
        if let Some(enabled) = read_bool(state, root, crate::settings::ADBLOCK_PATH) {
            next.adblock_enabled = enabled;
        }
        if let Some(enabled) = read_bool(state, root, crate::settings::CLEAR_ON_EXIT_PATH) {
            next.clear_on_exit = enabled;
        }
        if let Some(value) = read_text(state, root, crate::settings::HOMEPAGE_PATH)
            && is_supported_web_url(&value)
        {
            next.homepage = value;
        }
        if let Some(value) = read_text(state, root, crate::settings::SEARCH_ENGINE_PATH)
            && is_supported_web_url(&value)
        {
            next.search_engine = value;
        }
        if next != self.settings {
            if next.adblock_enabled != self.settings.adblock_enabled {
                self.set_adblock(next.adblock_enabled);
            }
            if next.clear_on_exit != self.settings.clear_on_exit {
                self.engine
                    .send(WebViewCommand::SetClearOnExit(next.clear_on_exit));
            }
            self.settings = next;
            if !self.headless && self.browsing_mode.persists_personal_state() {
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
    /// user may be editing; focus alone does not imply an unsent draft).
    fn url_input_focused(&self) -> bool {
        self.control_focused(URL_INPUT_ID)
    }

    fn control_focused(&self, id: &str) -> bool {
        fn walk(w: &dyn Widget, id: &str) -> Option<bool> {
            if w.test_id() == Some(id) {
                return Some(w.is_focused());
            }
            for c in w.children() {
                if let Some(f) = walk(c, id) {
                    return Some(f);
                }
            }
            None
        }
        self.composed
            .as_ref()
            .and_then(|c| walk(c.as_ref(), id))
            .unwrap_or(false)
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
        let editing = self.url_input_focused();
        for event in self
            .engine
            .drain_events()
            .into_iter()
            .chain(self.engine.drain_download_events())
        {
            let event = match event {
                WebViewEvent::LoadFinishedAt { uri } => {
                    if self.navigation_deferred || uri != self.current_uri {
                        continue;
                    }
                    WebViewEvent::LoadFinished
                }
                event => event,
            };
            match event {
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
                    if !self.navigation_deferred
                        && (uri != "about:blank"
                            || self.initial_url == "about:blank"
                            || self.session_first_commit)
                    {
                        self.session_url.clone_from(&uri);
                        self.session_first_commit = true;
                    }
                    self.document_revision = self.document_revision.wrapping_add(1);
                    self.favicon = None;
                    self.media_playing = false;
                    self.media_document_generation = None;
                    if uri != self.current_uri {
                        self.title.clear();
                        self.loading_stopped = false;
                        self.structure_dirty |= self.daily.navigation_started(&self.engine);
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
                WebViewEvent::LoadProgress(p) if !self.forced_loading && !self.loading_stopped => {
                    self.set_loading_progress(p);
                }
                WebViewEvent::LoadFinishedAt { .. }
                | WebViewEvent::FaviconChanged { .. }
                | WebViewEvent::TitleChanged(_)
                | WebViewEvent::LoadProgress(_) => {}
                WebViewEvent::LoadFinished => {
                    if !self.forced_load_error && !self.address_error_active {
                        self.load_error = None;
                    }
                    if !self.forced_loading {
                        self.set_loading_progress(1.0);
                    }
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if !self.headless
                        && self.browsing_mode.persists_personal_state()
                        && !self.history_paused()
                        && self.current_uri.starts_with("http")
                    {
                        self.history = History::load();
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
                    self.page_navigation_settled(None);
                }
                WebViewEvent::LoadFailed { uri, error } => {
                    self.address_navigation_value = None;
                    self.set_loading_progress(1.0);
                    tracing::warn!("load failed for {uri}: {error}");
                    self.title = format!("Failed to load {uri}");
                    self.load_error = Some(error.chars().take(240).collect());
                    self.structure_dirty = true;
                    self.page_navigation_settled(Some(&error));
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

    /// Anchor Picture-in-Picture feedback to the overflow menu, the control
    /// that holds the command, at every window width: a no-video explanation
    /// stays visible (and clamped) rather than being silently dropped.
    fn pip_feedback_anchor(&self) -> Option<Rect> {
        self.find_rect(toolbar::OVERFLOW_ID)
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
        theme: &Theme,
    ) -> Option<Rect> {
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s3);
        let available_width = viewport.width() - inset * 2.0;
        if available_width <= 0.0 {
            return None;
        }
        let desired_width = f64::from(metrics.panel_width_list);
        let width = desired_width.min(available_width);
        // One row of text, plus a row of actions when the card asks for a
        // decision.
        let row = f64::from(metrics.panel_row_height);
        let height = if has_actions {
            2.0 * row + f64::from(spacing.s4)
        } else {
            row + f64::from(spacing.s4)
        };
        let left_bound = viewport.x0 + inset;
        let right_bound = (viewport.x1 - inset - width).max(left_bound);
        let x0 = (anchor.x1 - width).clamp(left_bound, right_bound);
        let top_bound = viewport.y0 + inset;
        let bottom_bound = (viewport.y1 - inset - height).max(top_bound);
        let y0 = (anchor.y1 + f64::from(spacing.s2)).clamp(top_bound, bottom_bound);
        Some(Rect::new(x0, y0, x0 + width, y0 + height))
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
            let x = anchor.x0.min((self.rect.x1 - 220.0).max(self.rect.x0));
            self.back_menu.open_at([x as f32, (anchor.y1 + 4.0) as f32]);
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
                    if self.is_framework_hosted() {
                        if let Some(action) = self.page_bookmark_action() {
                            self.enqueue_library_action(action);
                        }
                    } else {
                        self.queue_unit_operation("browser.page", "bookmark");
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
            if widget.test_id() == Some(id) {
                widget.set_focus(true);
                if let Some((_, text)) = widget.text_review() {
                    if text.is_empty() {
                        ui::undo::set_state(widget, "0");
                    } else {
                        ui::undo::set_state(widget, &format!("{}:b:0", text.len()));
                    }
                }
                return true;
            }
            for child in widget.children_mut() {
                if focus(child, id) {
                    return true;
                }
            }
            false
        }
        self.composed.as_mut().is_some_and(|composed| {
            clear(composed.as_mut());
            focus(composed.as_mut(), id)
        })
    }

    fn focus_control_preserving_selection(&mut self, id: &str) {
        fn state(widget: &dyn Widget, id: &str) -> Option<String> {
            if widget.test_id() == Some(id) {
                return ui::undo::get_state(widget);
            }
            widget.children().iter().find_map(|child| state(*child, id))
        }

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
            .as_ref()
            .and_then(|widget| state(widget.as_ref(), id));
        self.focus_control(id);
        if let (Some(value), Some(widget)) = (saved, self.composed.as_mut()) {
            restore(widget.as_mut(), id, &value);
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

/// Live browsing always uses a real engine. WPE is an explicit migration
/// option; deterministic rendering is confined to `new_headless`.
fn default_engine(mode: BrowsingMode) -> SharedEngine {
    #[cfg(feature = "wpe")]
    if std::env::var("TILO_BROWSER_ENGINE").as_deref() == Ok("wpe") {
        if mode.is_private() {
            return engine_or_error(
                crate::storage::explicit_root()
                    .ok_or_else(|| "Private browser root was not configured".to_string())
                    .and_then(|root| {
                        ui::webview::chromium::UnavailableEngine::new_with_private_root(
                            "Private windows require the Chromium engine; WPE private storage is unavailable."
                                .into(),
                            root,
                        )
                    })
                    .map(|engine| engine as SharedEngine),
            );
        }
        return engine_or_error(
            ui::webview::wpe::WpeEngine::new().map(|engine| engine as SharedEngine),
        );
    }
    if mode.is_private() {
        return engine_or_error(
            crate::storage::explicit_root()
                .ok_or_else(|| "Private browser root was not configured".to_string())
                .and_then(ui::webview::chromium::ChromiumEngine::new_private)
                .map(|engine| engine as SharedEngine),
        );
    }
    if let Some(root) = crate::storage::explicit_root() {
        return engine_or_error(
            ui::webview::chromium::ChromiumEngine::with_profile_extensions(
                &root.join("chromium"),
                &crate::extensions::approved_paths(),
            )
            .map(|engine| engine as SharedEngine),
        );
    }
    engine_or_error(
        ui::webview::chromium::ChromiumEngine::new_with_extensions(
            &crate::extensions::approved_paths(),
        )
        .map(|engine| engine as SharedEngine),
    )
}

fn engine_or_error(result: Result<SharedEngine, String>) -> SharedEngine {
    result.unwrap_or_else(|error| {
        tracing::error!(%error, "browser engine unavailable");
        ui::webview::chromium::UnavailableEngine::new(error)
    })
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

impl Continuable for BrowserWidget {
    fn continuation_metadata(
        &self,
        _state: &AutoCommit,
        _root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        if self.browsing_mode.is_private() {
            return None;
        }
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
        if self.browsing_mode.is_private() {
            return false;
        }
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
    ui::provides!(
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[BROWSER_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
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
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if self.private_window_request.get() {
            self.private_window_request.set(false);
            self.private_window_notice = crate::private_window::launch().err();
            self.structure_dirty = true;
        }
        let paused = self.history_paused();
        if self.observed_history_paused != paused {
            self.observed_history_paused = paused;
            self.structure_dirty = true;
        }
        if !self.bootstrap_done {
            self.site.initialize(
                &self.engine,
                !self.headless && self.browsing_mode.persists_personal_state(),
            );
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
                if !self.navigation_deferred {
                    self.engine
                        .send(WebViewCommand::Navigate(self.initial_url.clone()));
                }
            }
            self.bootstrap_done = true;
        }

        if let Some(message) = self
            .extension_feedback
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.extension_notice = Some(message);
            self.structure_dirty = true;
        }
        self.structure_dirty |= self.daily.sync(&self.engine);
        self.structure_dirty |= self.run_pending_commands();
        let site_was_open = self.site.open;
        self.structure_dirty |= self.site.sync(&self.engine);
        if self.site.open && !site_was_open {
            self.deactivate_credentials();
        }
        self.structure_dirty |= self.credentials.sync(&self.engine);
        if self.credentials.open {
            self.site.close();
        }
        self.tick_back_hold(ui::time::now());
        self.apply_engine_events();
        self.drive_page_operation();
        self.structure_dirty |= self.capabilities.downloads_changed();
        self.flush_library_edits(state);
        let heads = state.get_heads();
        if self.projected_heads.as_ref() != Some(&heads) {
            self.sync_settings_from_document(state, path);
            self.sync_bookmarks(state, path);
            self.projected_heads = Some(heads);
        }
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
        self.pending_chrome_focus = None;
        let page_focused = self.control_focused("browser.surface");
        let address_state = self
            .composed
            .as_ref()
            .and_then(|widget| address_state(widget.as_ref()));
        // The tiers and the labelled widths follow the text scale, which is
        // only known here; the one-frame capture seed set them at 1×.
        self.text_scale = theme.text_scale;
        self.compact_breakpoint = theme.tokens.metrics.compact_breakpoint;
        if self.rect.width() > 0.0 {
            self.compact_chrome = is_compact_chrome(self.chrome_width(), self.compact_breakpoint);
        }
        let mut composed = self.build_composition(theme);
        let child_node = composed.build_layout(taffy, theme, node_map);
        if let Some((id, state)) = address_state {
            restore_address(composed.as_mut(), &id, &state);
        }
        composed.set_motion_reduced(self.motion_reduced);
        self.composed = Some(composed);
        if let Some(id) = self.daily.focus_pending.take() {
            self.focus_control(id);
            if id == crate::daily_controls::FIND_INPUT
                && let Some(cursor) = self.daily.take_buffered_cursor()
                && let Some(composed) = self.composed.as_mut()
            {
                restore_address(composed.as_mut(), id, &cursor.to_string());
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
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child_node` was just built in this one above.
        let mut children = vec![child_node];
        self.site_panel = None;
        if self.site.open || self.credentials.open {
            // The panel hangs directly under the toolbar row — its controls'
            // height plus the toolbar Container's padding either side, the
            // same sum the toolbar itself lays out to, so the two agree at
            // every UI scale — with one `s3` gutter from the window's left
            // edge (and the same kept free on the right) and `s12` kept free
            // at the foot.
            let gutter = theme.tokens.spacing.s3;
            let top = ControlSize::Regular.height(theme) as f32 + 2.0 * theme.tokens.spacing.s1;
            let width = (self.rect.width() as f32 - 2.0 * gutter).clamp(240.0, 480.0);
            let height =
                (self.rect.height() as f32 - top - theme.tokens.spacing.s12).clamp(180.0, 510.0);
            let mut panel = if self.credentials.open {
                self.credentials.panel(width, height)
            } else {
                self.site.panel(width, height)
            };
            let panel_node = panel.build_layout(taffy, theme, node_map);
            #[allow(
                clippy::unwrap_used,
                reason = "panel node was just created in this tree"
            )]
            let mut panel_style = taffy.style(panel_node).unwrap().clone();
            panel_style.position = taffy::Position::Absolute;
            panel_style.inset.left = taffy::LengthPercentageAuto::length(gutter);
            panel_style.inset.top = taffy::LengthPercentageAuto::length(top);
            let _ = taffy.set_style(panel_node, panel_style);
            children.push(panel_node);
            self.site_panel = Some(panel);
        }
        #[allow(
            clippy::unwrap_used,
            reason = "all child nodes were just created in this tree"
        )]
        let node = taffy.new_with_children(style, &children).unwrap();
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
        let compact = is_compact_chrome(self.chrome_width(), self.compact_breakpoint);
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
        if let Some(panel) = &mut self.site_panel
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
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
        // One hairline under the toolbar row separates it from the page.
        if let Some(rect) = self.find_rect("browser.toolbar") {
            ui::widget::toolbar::paint_band(scene, transform, theme, rect);
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
        if let Some(panel) = &mut self.site_panel {
            // A floating panel: its own material and shadow, with its content
            // inked for that ground.
            ui::widget::floating_surface::paint(scene, transform, panel.get_rect(), theme);
            let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
            panel.draw(scene, transform, text_renderer, &on_panel, presence);
            panel.draw_overlay(scene, transform, text_renderer, &on_panel, presence);
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
                let Some(rect) =
                    Self::pip_feedback_rect_for_anchor(self.rect, anchor, has_actions, theme)
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
        if self.navigation_hold.waiting() {
            return true;
        }
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.window_scale = ctx.scale_factor as f32;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let Some((node, id)) = self.pending_chrome_focus.take() {
            self.focus_control_preserving_selection(id);
            *ctx.focus_target = Some(node);
        }
        if self.site.open || self.credentials.open {
            let dismiss = matches!(event,WindowEvent::KeyboardInput {event:key,..} if key.state==ElementState::Pressed && key.logical_key==WKey::Named(WNamedKey::Escape))
                || matches!(event,WindowEvent::MouseInput {state:ElementState::Pressed,..} if self.site_panel.as_ref().is_some_and(|p|!p.get_rect().contains(self.cursor)));
            if dismiss {
                self.site.close();
                self.deactivate_credentials();
                self.structure_dirty = true;
                *ctx.handled = true;
                return;
            }
            if let Some(panel) = &mut self.site_panel
                && let Ok(children) = ctx.taffy.children(ctx.node_id)
                && let Some(&panel_node) = children.get(1)
            {
                panel.handle_event(event, &mut ctx.reborrow(ctx.path, panel_node));
            }
            *ctx.handled = true;
            return;
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
                *ctx.handled = true;
                return;
            }
            if self.pip_float_page_rect.contains(self.cursor) {
                self.float_current_page();
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
            if let Some(entry) = self.suggestions.take_chosen() {
                self.suggestions.clear();
                self.last_query = None;
                self.address.set(entry.url.clone());
                self.queue_navigate(entry.url);
                *ctx.handled = true;
                return;
            }
            if ov_handled {
                *ctx.handled = true;
                return;
            }
        }

        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
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
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.back_menu.hovered() == Some(index),
                        ..Default::default()
                    })
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if self
            .suggestions
            .perform_accessibility_action(target, action)
        {
            if let Some(entry) = self.suggestions.take_chosen() {
                self.suggestions.clear();
                self.last_query = None;
                self.address.set(entry.url.clone());
                // This queues Browser's existing typed `browser.page/navigate`
                // action — AT-SPI does not get a parallel imperative navigation
                // route.
                self.queue_navigate(entry.url);
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
        self.site
            .deadline()
            .into_iter()
            .chain(self.credentials.deadline())
            .chain(
                self.children()
                    .into_iter()
                    .filter_map(Widget::next_wake_deadline),
            )
            .min()
    }
    fn modal_capture(&self) -> Option<Rect> {
        if self.site.open || self.credentials.open {
            return self.site_panel.as_ref().map(|p| p.get_rect());
        }
        self.page_menu
            .menu
            .modal_capture()
            .or_else(|| self.back_menu.modal_capture())
            .or_else(|| self.suggestions.modal_capture())
            .or_else(|| self.composed.as_ref().and_then(|c| c.modal_capture()))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .chain(self.site_panel.as_ref().map(|p| p.as_ref() as &dyn Widget))
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .chain(
                self.site_panel
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

impl ui::search::capability::SearchSource for BrowserWidget {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::resources::PublishesResources for BrowserWidget {
    #[allow(
        clippy::too_many_lines,
        reason = "Resource definitions stay together so their action contracts remain discoverable."
    )]
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        const MATERIALIZED_LIMIT: usize = 100;
        // Hosted: ResourceRoot already published bookmarks + window navigation
        // via tilo_resource::publish. Live extras (find-in-page session, load
        // progress) are not resource actions today, so there is nothing left
        // to add beside that door.
        if self.is_framework_hosted() {
            return Vec::new();
        }
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
            (
                "stop",
                "Stop loading",
                BrowserOperation::Stop,
                self.progress < 1.0,
                "Page is idle",
            ),
        ] {
            let payload = browser_operation_payload(&operation);
            if let Ok(spec) = ActionSpec::operation(
                id,
                label,
                OperationKind::View,
                BROWSER_OPERATION_TYPE,
                &payload,
            ) {
                let spec = if id == "stop" {
                    spec
                } else {
                    spec.with_contract(navigation_contract())
                };
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
        page.actions.extend(self.page_content_actions());

        let mut session = Resource::new(
            "browser.session",
            "browser.history-pause",
            if self.history_paused() {
                "History paused"
            } else {
                "History recording"
            },
        );
        if let Some(spec) = operation(
            "toggle-history-pause",
            if self.history_paused() {
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
        let documented_bookmarks: std::collections::HashSet<String> =
            crate::actions::bookmarks_from_doc(doc)
                .map(|book| {
                    book.entries()
                        .iter()
                        .map(|entry| bookmark_resource_id(&entry.url))
                        .collect()
                })
                .unwrap_or_default();
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
            // Remove is the published document action. A bookmark that exists
            // only in this window's file cache still needs it here.
            if !documented_bookmarks.contains(child.id.as_str())
                && let Ok(spec) = ActionSpec::immediate(
                    "remove",
                    "Remove bookmark",
                    BROWSER_ACTION_TYPE,
                    &BrowserAction::RemoveBookmark {
                        url: entry.url.clone(),
                        observed_tags: crate::actions::observed_bookmark_tags(doc, &entry.url),
                    },
                )
            {
                child.actions.push(spec);
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
        // same section operations as the visible surface.
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
        let state = <crate::resource::Browser as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut browser =
            ui::tilo_resource::publish::<crate::resource::Browser>("browser", "Browser", &state);
        if !browser
            .actions
            .iter()
            .any(|action| action.id == "show-page")
            && let Some(spec) = operation(
                "show-page",
                "Show current page",
                OperationKind::View,
                BrowserOperation::ShowPage,
            )
        {
            browser.actions.push(spec);
        }
        let published = std::mem::take(&mut browser.children);
        let mut bookmarks = bookmarks;
        let mut settings = None;
        let mut extra = Vec::new();
        for document in published {
            if document.id.as_str() == "browser.bookmarks" {
                for action in document.actions {
                    if !bookmarks.actions.iter().any(|have| have.id == action.id) {
                        bookmarks.actions.push(action);
                    }
                }
                let hand_children = std::mem::take(&mut bookmarks.children);
                bookmarks.children = document.children;
                for hand in hand_children {
                    if let Some(existing) = bookmarks
                        .children
                        .iter_mut()
                        .find(|child| child.id == hand.id)
                    {
                        for action in hand.actions {
                            if !existing.actions.iter().any(|have| have.id == action.id) {
                                existing.actions.push(action);
                            }
                        }
                    } else {
                        bookmarks.children.push(hand);
                    }
                }
            } else if document.id.as_str() == "browser.settings" {
                settings = Some(document);
            } else {
                extra.push(document);
            }
        }
        let settings = settings.unwrap_or_else(settings_resource);
        browser.children = vec![
            page, session, history, bookmarks, downloads, extensions, library, settings,
        ];
        browser.children.extend(extra);
        vec![browser]
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        if self.is_framework_hosted() {
            if id == "browser" {
                return None;
            }
            let state = <crate::resource::Browser as ui::tilo_resource::Resource>::read(doc)
                .unwrap_or_default();
            let root = ui::tilo_resource::publish::<crate::resource::Browser>(
                "browser", "Browser", &state,
            );
            return find(&[root], id);
        }
        find(&self.resources(doc), id)
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
    fn answer_resource_operation(
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

    fn admit_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind == OperationKind::Document || type_id != BROWSER_OPERATION_TYPE {
            return Ok(None);
        }
        let operation = decode_browser_operation(payload)?;
        if let BrowserOperation::Page(request) = operation {
            self.start_page_operation(invocation, request)?;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
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
                local_output(if self.history_paused() {
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
    use ui::webview::WebViewEngine;

    fn compose(widget: &mut BrowserWidget) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&mut doc, &root);
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
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

    fn assert_action_binding(
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

    #[test]
    #[ignore = "manual hot-path benchmark"]
    fn bookmark_projection_benchmark() {
        use automerge::transaction::Transactable;
        let mut doc = AutoCommit::new();
        for i in 0..1000 {
            let row = doc
                .put_object(
                    automerge::ROOT,
                    format!("browser.bookmarks.v2.add:{i}"),
                    automerge::ObjType::Map,
                )
                .unwrap();
            doc.put(&row, "url", format!("https://example.test/{i}"))
                .unwrap();
            doc.put(&row, "title", format!("Page {i}")).unwrap();
        }
        let mut widget = BrowserWidget::new_headless();
        widget.sync(&mut doc, &automerge::ROOT);
        assert_eq!(widget.bookmarks.entries().len(), 1000);
        let started = ui::time::now();
        for _ in 0..100 {
            widget.sync(&mut doc, &automerge::ROOT);
        }
        let elapsed = started.elapsed();
        eprintln!("100 browser syncs / 1000 bookmarks: {elapsed:?}");
        let reference = ui::time::now();
        for _ in 0..100 {
            std::hint::black_box(crate::actions::bookmarks_from_doc(&doc));
        }
        let uncached = reference.elapsed();
        assert!(
            elapsed < uncached / 5,
            "unchanged frame sync {elapsed:?} should avoid full projection {uncached:?}"
        );
    }

    fn dispatch_back_event(widget: &mut BrowserWidget, event: &WindowEvent) -> bool {
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        widget.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    fn mouse(button: MouseButton, state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            button,
            state,
        }
    }

    fn key(named: WNamedKey) -> WindowEvent {
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = WKey::Named(named);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
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
            let mut tree = TaffyTree::new();
            let node = tree.new_leaf(Style::default()).unwrap();
            widget.handle_event(
                &WindowEvent::Focused(true),
                &mut ui::widget::EventCtx {
                    scale_factor: 2.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut AutoCommit::new(),
                    path: &automerge::ROOT,
                    handled: &mut false,
                    focus_target: &mut None,
                    node_id: node,
                    taffy: &tree,
                },
            );
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
                widget.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &automerge::ROOT,
                        handled: &mut false,
                        focus_target: &mut target,
                        node_id: node,
                        taffy: &tree,
                    },
                );
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
            widget.resource_navigation_invocation = Some(invocation);
            widget.set_loading_progress(0.2);
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&widget, &doc);
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        stop.kind,
                        &encoded.type_id,
                        &encoded.payload,
                    ),
                    &mut doc,
                    &automerge::ROOT,
                )
                .unwrap();
            let Some(ui::widget::ResourceOperationStart::Completed(output)) = result else {
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
            let mut composed = widget.build_composition(&Theme::dark());
            composed.set_motion_reduced(true);
            let mut theme = Theme::dark();
            theme.reduce_motion = true;
            composed.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::default(),
            );
            assert!(!composed.needs_animation_frame());
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
            let composed = widget.build_composition(&Theme::dark());
            assert!(
                composed.needs_animation_frame(),
                "loading indicator animates immediately"
            );
            widget.apply_engine_events();
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            // Paint the new engine frame once before checking idle scheduling.
            let mut composed = widget.build_composition(&Theme::dark());
            composed.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert!(!composed.needs_animation_frame());
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.decision.false.1.true").unwrap(),
                    ),
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

        // Phase `cached_document_projection_observes_new_bookmark_actions`.
        {
            use automerge::transaction::Transactable;
            let mut widget = BrowserWidget::new_headless();
            let mut doc = AutoCommit::new();
            widget.sync(&mut doc, &automerge::ROOT);
            let row = doc
                .put_object(
                    automerge::ROOT,
                    "browser.bookmarks.v2.add:one",
                    automerge::ObjType::Map,
                )
                .unwrap();
            doc.put(&row, "url", "https://new.test").unwrap();
            doc.put(&row, "title", "New").unwrap();
            widget.sync(&mut doc, &automerge::ROOT);
            assert!(widget.bookmarks.contains("https://new.test"));
            doc.put(automerge::ROOT, "browser.bookmarks.v2.remove:one", true)
                .unwrap();
            widget.sync(&mut doc, &automerge::ROOT);
            assert!(!widget.bookmarks.contains("https://new.test"));
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
                ("browser.bookmark", "browser.page", "bookmark"),
                ("browser.library", "browser.history", "show"),
            ] {
                assert_action_binding(&page, control, resource, action);
            }
            // The overflow menu's commands queue the same canonical actions.
            for (resource, action) in [
                ("browser.session", "toggle-history-pause"),
                ("browser.page", "picture-in-picture"),
                ("browser.settings", "show"),
            ] {
                let mut page = BrowserWidget::new_headless();
                compose(&mut page);
                page.queue_command(resource, action)();
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
            compose(&mut library);
            assert_no_document_control_paths(&library);
            for (control, resource, action) in [
                ("browser.library.close", "browser", "show-page"),
                (
                    "browser.library.history-pause",
                    "browser.session",
                    "toggle-history-pause",
                ),
            ] {
                assert_action_binding(&library, control, resource, action);
            }

            let mut settings = BrowserWidget::new_headless();
            settings.show_settings = true;
            compose(&mut settings);
            assert_no_document_control_paths(&settings);
            for (control, resource, action) in [
                ("settings.close", "browser", "show-page"),
                (
                    "browser.credentials.open",
                    "browser.settings",
                    "open-passwords",
                ),
                ("browser.address.open", "browser.settings", "open-addresses"),
                (
                    "settings.new_private_window",
                    "browser.settings",
                    "new-private-window",
                ),
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
                &WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: winit::dpi::PhysicalPosition::new(850.0, 650.0),
                },
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
                &mut AutoCommit::new(),
                &automerge::ROOT
            ));
            assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
            assert!(!widget.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut AutoCommit::new(),
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
                &mut AutoCommit::new(),
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
            for (section, scope) in [
                ("history", "browser.history"),
                ("bookmarks", "browser.bookmarks"),
                ("downloads", "browser.downloads"),
            ] {
                let mut widget = BrowserWidget::new_headless();
                widget.debug_show_library(section);
                compose(&mut widget);
                let control = format!("search-open-{scope}");
                assert!(
                    ui::widget::controls::control_action_bindings(&widget)
                        .iter()
                        .any(|binding| binding.control_id == control),
                    "the {section} section offers search scoped to it"
                );
            }
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
            ui::resources::resources(widget, doc)[0]
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
            match widget.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    action.kind,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                doc,
                root,
            )? {
                Some(ui::widget::ResourceOperationStart::Accepted) => Ok(invocation),
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
            for id in ["read", "click", "fill", "select", "press", "submit"] {
                let action = page_action(&widget, &doc, id);
                assert!(action.contract.validate_declaration().is_empty(), "{id}");
                assert!(action.contract.availability.is_available(), "{id}");
                assert!(action.contract.is_programmatic(), "{id}");
                assert_eq!(
                    action.contract.permission,
                    ui::tilo_actions::PermissionRequirement::Write,
                    "reading a signed-in page takes the authority to operate it"
                );
                assert_eq!(
                    action.contract.execution,
                    ui::tilo_actions::ExecutionMode::Background { cancellable: false }
                );
            }
            let (_, blank, doc, _) = page_on("about:blank");
            assert!(
                !page_action(&blank, &doc, "read")
                    .contract
                    .availability
                    .is_available()
            );
        }

        // Phase `read_returns_the_engine_outline_in_the_declared_shape`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            let invocation =
                start(&mut widget, &mut doc, &root, "read", &serde_json::json!({})).unwrap();
            assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
            widget.sync(&mut doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                widget.poll_resource_operation(invocation, false, &doc, &root)
            else {
                panic!("an answered read must succeed its invocation");
            };
            let read = page_action(&widget, &doc, "read");
            assert!(read.contract.result.validate(&output).is_ok());
            let value = output.value.unwrap();
            assert_eq!(value["url"], "https://shop.test/");
            assert_eq!(
                value["outline"],
                "- document \"https://shop.test/ — test page\""
            );
        }

        // Phase `a_read_on_a_loading_page_waits_for_it_to_settle`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/");
            widget.set_loading_progress(0.4);
            let invocation =
                start(&mut widget, &mut doc, &root, "read", &serde_json::json!({})).unwrap();
            widget.sync(&mut doc, &root);
            assert!(
                page_requests(&engine).is_empty(),
                "read before the load settled"
            );
            assert!(
                widget
                    .poll_resource_operation(invocation, false, &doc, &root)
                    .is_none()
            );
            engine.send(WebViewCommand::Reload);
            widget.sync(&mut doc, &root);
            assert_eq!(page_requests(&engine), vec![PageRequest::Outline]);
            widget.sync(&mut doc, &root);
            assert!(matches!(
                widget.poll_resource_operation(invocation, false, &doc, &root),
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
                "fill",
                &serde_json::json!({"element": "d3e7", "text": ""}),
            )
            .unwrap();
            widget.sync(&mut doc, &root);
            assert!(
                widget
                    .poll_resource_operation(fill, false, &doc, &root)
                    .is_some()
            );
            let select = start(
                &mut widget,
                &mut doc,
                &root,
                "select",
                &serde_json::json!({"element": "d3e8", "values": ["NL", "Belgium"]}),
            )
            .unwrap();
            widget.sync(&mut doc, &root);
            assert!(
                widget
                    .poll_resource_operation(select, false, &doc, &root)
                    .is_some()
            );
            let press = start(
                &mut widget,
                &mut doc,
                &root,
                "press",
                &serde_json::json!({"key": "Shift+Tab"}),
            )
            .unwrap();
            widget.sync(&mut doc, &root);
            assert!(
                widget
                    .poll_resource_operation(press, false, &doc, &root)
                    .is_some()
            );
            assert_eq!(
                page_requests(&engine),
                vec![
                    PageRequest::Fill {
                        element: "d3e7".into(),
                        text: String::new(),
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
                ("click", serde_json::json!({"element": "#checkout"})),
                ("submit", serde_json::json!({"element": ""})),
                ("press", serde_json::json!({"key": "Hyper+x"})),
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

        // Phase `an_action_that_navigates_completes_on_the_page_it_opened`.
        {
            let (engine, mut widget, mut doc, root) = page_on("https://shop.test/cart");
            engine.push_page_reply(Ok(PageReply {
                url: "https://shop.test/cart".into(),
                message: "Clicked button \"Checkout\"".into(),
                outline: None,
                navigated: true,
                loading: true,
            }));
            let click = start(
                &mut widget,
                &mut doc,
                &root,
                "click",
                &serde_json::json!({"element": "d1e4"}),
            )
            .unwrap();
            widget.sync(&mut doc, &root);
            assert!(
                widget
                    .poll_resource_operation(click, false, &doc, &root)
                    .is_none(),
                "a click that started a navigation waits for it"
            );
            assert!(
                start(&mut widget, &mut doc, &root, "read", &serde_json::json!({})).is_err(),
                "a second page action is refused while one runs"
            );
            engine.send(WebViewCommand::Navigate(
                "https://shop.test/checkout".into(),
            ));
            widget.sync(&mut doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                widget.poll_resource_operation(click, false, &doc, &root)
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
                "click",
                &serde_json::json!({"element": "d1e4"}),
            )
            .unwrap();
            widget.sync(&mut doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
                widget.poll_resource_operation(click, false, &doc, &root)
            else {
                panic!("a refused click must fail its invocation");
            };
            assert_eq!(error.code, "browser.page.stale_element");
            assert!(!error.retryable);
            assert!(start(&mut widget, &mut doc, &root, "read", &serde_json::json!({})).is_ok());
        }
    }

    fn history_save_fails(_: &History) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test history directory is not writable",
        ))
    }

    pub(super) fn layout_at(widget: &mut BrowserWidget, width: f32, height: f32) {
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
                w.on_nav_gesture(NavDirection::Back, &mut state, &root),
                "a back gesture is consumed by the embedded WebView",
            );
            assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
            w.on_nav_gesture(NavDirection::Forward, &mut state, &root);
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
            w.sync(&mut state, &root);

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
            w.sync(&mut state, &root);

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

    #[test]
    fn history_clear_on_exit_belongs_to_workspace_lifetime() {
        const CHILD: &str = "TILO_TEST_HISTORY_EXIT_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let path = crate::history::history_path().unwrap();
            assert!(
                path.starts_with(std::path::PathBuf::from(root)),
                "isolated history path"
            );
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.headless = false;
            w.settings.clear_on_exit = true;
            w.history.record_at("https://retained.test", "Retained", 1);
            w.history.save().unwrap();
            drop(w);
            assert!(
                !History::load().is_empty(),
                "closing one page must retain shared history"
            );
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.headless = false;
            w.settings.clear_on_exit = true;
            drop(crate::BrowserTabs::new(w));
            assert!(
                History::load().is_empty(),
                "workspace exit clears persisted history"
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "browser_widget::tests::history_clear_on_exit_belongs_to_workspace_lifetime",
                "--nocapture",
            ])
            .env(CHILD, root.path())
            .env("HOME", root.path())
            .env("XDG_DATA_HOME", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "the child ran no test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
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
                                        && address.action == "toggle-history-pause"
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
            assert!(
                (120.0..=160.0).contains(&history_tab.x0),
                "desktop title, tabs, and rows share a centered 920px Library measure"
            );
            assert!(
                first.x1 < 1_080.0,
                "desktop row actions stay at the trailing edge of the centered Library measure"
            );
            assert!(first.height() <= 60.0);
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
            assert!(
                (120.0..=160.0).contains(&history_tab.x0),
                "large text retains the desktop Library measure instead of becoming edge-to-edge"
            );
            assert!(
                open.x1 < 1_080.0,
                "large-text row actions stay inside the centered Library measure"
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
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("addresses.json");
            let mut widget = BrowserWidget::new_headless();
            widget.credentials =
                crate::credentials::Credentials::with_address_path(false, Some(path.clone()));
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
                if id == "browser.address.open" {
                    widget
                        .perform_browser_operation(BrowserOperation::ShowAutofill {
                            addresses: true,
                        })
                        .unwrap();
                } else {
                    let result = widget
                        .start_operation(
                            &ui::widget::ResourceOperation::local(
                                ui::tilo_actions::InvocationId::new(),
                                OperationKind::Effect,
                                "tilo.ui.control.invoke",
                                &serde_json::to_vec(id).unwrap(),
                            ),
                            &mut doc,
                            &root,
                        )
                        .unwrap();
                    assert!(
                        result.is_some(),
                        "address editor control must be reachable: {id}"
                    );
                }
                assert!(widget.credentials.sync(&widget.engine));
                widget.structure_dirty = true;
                layout_at(&mut widget, 390.0, 700.0);
                if id == "browser.address.open" {
                    assert!(!path.exists(), "opening does not opt in or write");
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
            let store = crate::autofill::AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
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
                if id.ends_with("open") {
                    // The settings row runs the canonical action.
                    widget
                        .perform_browser_operation(BrowserOperation::ShowAutofill {
                            addresses: false,
                        })
                        .unwrap();
                } else {
                    let result = widget
                        .start_operation(
                            &ui::widget::ResourceOperation::local(
                                ui::tilo_actions::InvocationId::new(),
                                OperationKind::Effect,
                                "tilo.ui.control.invoke",
                                &serde_json::to_vec(id).unwrap(),
                            ),
                            &mut doc,
                            &root,
                        )
                        .unwrap();
                    assert!(
                        result.is_some(),
                        "credential action must reach its visible control"
                    );
                }
                assert!(widget.credentials.sync(&widget.engine));
                widget.structure_dirty = true;
                layout_at(&mut widget, 390.0, 700.0);
                if id.ends_with("open") {
                    assert!(widget.credentials.open);
                    let panel = widget.site_panel.as_ref().unwrap();
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
            let mut doc = AutoCommit::new();
            widget.sync(&mut doc, &automerge::ROOT);
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            assert_eq!(address_for(&widget, "browser.library").action, "show");

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
            let tabs = crate::browser_tabs::BrowserTabs::new(widget);
            let mut harness = ui::headless::FrameHarness::new(
                ui::state::State::new(),
                Box::new(tabs),
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
                let panel = widget.site_panel.as_ref().unwrap();
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
                    // The panel's page switcher opens the connection page.
                    widget.site.open_command(true)();
                    widget.site.sync(&widget.engine);
                    layout_at(&mut widget, 390.0, 700.0);
                }
                let rect = widget.site_panel.as_ref().unwrap().get_rect();
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
            assert!(widget.site_panel.is_some());
            let result = widget
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.site.close").unwrap(),
                    ),
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
            widget.sync(&mut doc, &root);
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
            assert!(widget.site_panel.is_some());
            assert!(
                widget
                    .start_operation(
                        &ui::widget::ResourceOperation::local(
                            ui::tilo_actions::InvocationId::new(),
                            OperationKind::Effect,
                            "tilo.ui.control.invoke",
                            &serde_json::to_vec("browser.site.toggle").unwrap()
                        ),
                        &mut doc,
                        &root
                    )
                    .unwrap()
                    .is_some(),
                "the real protection button must accept the action"
            );
            widget.sync(&mut doc, &root);
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
            widget.sync(&mut doc, &root);
            widget.sync(&mut doc, &root);
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
        // Phase `narrow_pip_feedback_is_clamped_inside_viewport_safe_bounds`.
        {
            let viewport = Rect::new(0.0, 0.0, 390.0, 844.0);
            // Compact chrome has no dedicated PiP button; its semantic fallback
            // anchor is close enough to the trailing edge that trailing-only
            // alignment used to put the left side of this card offscreen.
            let anchor = Rect::new(278.0, 12.0, 322.0, 52.0);
            let theme = Theme::dark();
            let card = BrowserWidget::pip_feedback_rect_for_anchor(viewport, anchor, true, &theme)
                .unwrap();
            let inset = f64::from(theme.tokens.spacing.s3);
            assert!(card.x0 >= viewport.x0 + inset);
            assert!(card.x1 <= viewport.x1 - inset);
            assert!(card.y0 >= viewport.y0 + inset);
            assert!(card.y1 <= viewport.y1 - inset);
            assert!(
                (card.width() - f64::from(theme.tokens.metrics.panel_width_list)).abs()
                    < f64::EPSILON,
                "the full action card still fits"
            );
        }

        // Phase `compact_pip_feedback_uses_a_visible_toolbar_anchor`.
        {
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

        // Phase `reduced_motion_settles_pip_feedback_without_requesting_frames`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now(),
                persistent: false,
            });

            assert!(w.needs_animation_frame());
            w.set_motion_reduced(true);
            assert!(
                !w.needs_animation_frame(),
                "reduced motion keeps the feedback visible but does not animate its fade"
            );
        }

        // Phase `actionable_pip_feedback_does_not_expire_under_the_pointer`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now()
                    .checked_sub(PIP_FEEDBACK_LIFETIME + std::time::Duration::from_secs(1))
                    .unwrap_or_else(ui::time::Instant::now),
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
    }

    /// Picture-in-picture video probing, no-video fallback and host path.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pip_floats_the_video_or_falls_back_to_shell_selection() {
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
            w.sync(&mut state, &root);
            w.sync(&mut state, &root);

            assert!(w.take_floating_surface_request().is_none());
            assert_eq!(
                w.pip_feedback.as_ref().map(|feedback| feedback.kind),
                Some(PipFeedbackKind::NoVideo)
            );
        }

        // Phase `no_video_select_area_requests_the_shared_shell_pip_picker_once`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let (mut state, root) = synced(&mut w);
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now(),
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
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut state,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &tree,
                },
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
            let resources = ui::resources::resources(&w, &doc);
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
                w.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        pip.kind,
                        pip.operation_type.as_deref().unwrap(),
                        &browser_operation_payload(&BrowserOperation::TogglePictureInPicture)
                            .to_string()
                            .into_bytes()
                    ),
                    &mut doc,
                    &root
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
                    .start_operation(
                        &ui::widget::ResourceOperation::local(
                            invocation,
                            pip.kind,
                            pip.operation_type.as_deref().unwrap(),
                            &browser_operation_payload(&BrowserOperation::TogglePictureInPicture)
                                .to_string()
                                .into_bytes()
                        ),
                        &mut doc,
                        &root
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
            let engine: SharedEngine = engine_or_error(Err("Chromium unavailable".into()));
            let _clone: SharedEngine = Arc::clone(&engine);
            engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
            assert!(engine.frames().lock().unwrap().is_none());
            assert!(engine.drain_events().iter().any(|event| matches!(event,
                WebViewEvent::LoadFailed { error, .. } if error == "Chromium unavailable")));
        }

        // Phase `resource_surface_binds_typed_navigation_and_reports_history_availability`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut doc, root) = synced(&mut w);
            let resources = ui::resources::resources(&w, &doc);
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
            assert!(
                library
                    .actions
                    .iter()
                    .any(|action| action.id == "show-history"),
                "the library publishes its sections"
            );
            assert!(
                library
                    .actions
                    .iter()
                    .all(|action| action.id != "set-filter"),
                "searching the library is the platform search, not a library action"
            );
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
                w.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        OperationKind::Effect,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &root
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
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            widget.prepare_session_uri("https://example.test/saved".into(), false);
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

        // Phase `deferred_session_tab_keeps_saved_identity_before_activation`.
        {
            let mut widget = BrowserWidget::with_engine(TestEngine::new(), true);
            widget.prepare_session_uri("https://example.test/saved".into(), true);
            widget.title = "about:blank".into();
            widget.current_uri = "about:blank".into();
            assert_eq!(widget.tab_title(), "https://example.test/saved");
            assert_eq!(widget.persisted_url(), "https://example.test/saved");
        }

        // Phase `continuation_restore_navigates_the_matching_browser_instance`.
        {
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
            let resources = ui::resources::resources(&w, &doc);
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        clear_history.kind,
                        clear_history.operation_type.as_deref().unwrap(),
                        &browser_operation_payload(&BrowserOperation::ClearHistory)
                            .to_string()
                            .into_bytes(),
                    ),
                    &mut doc,
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        clear_site_data.kind,
                        clear_site_data.operation_type.as_deref().unwrap(),
                        &browser_operation_payload(&BrowserOperation::RequestWebsiteDataClear)
                            .to_string()
                            .into_bytes(),
                    ),
                    &mut doc,
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

        // Phase `history_pause_is_local_and_keeps_existing_history_searchable_and_clearable`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let (mut doc, root) = synced(&mut w);
            w.history
                .record_at("https://sensitive.example", "Sensitive", 1);
            let resources = ui::resources::resources(&w, &doc);
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        toggle.kind,
                        toggle.operation_type.as_deref().unwrap(),
                        &browser_operation_payload(&BrowserOperation::ToggleHistoryPause)
                            .to_string()
                            .into_bytes(),
                    ),
                    &mut doc,
                    &root,
                )
                .unwrap();
            let Some(ui::widget::ResourceOperationStart::Completed(output)) = started else {
                panic!("history pausing changes only this running browser");
            };
            assert!(toggle.contract.result.validate(&output).is_ok());
            let resources = ui::resources::resources(&w, &doc);
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

        // Phase `ui_history_clear_preserves_entries_and_surfaces_a_persistence_failure`.
        {
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

            let resources = ui::resources::resources(&w, &doc);
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
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        clear.kind,
                        clear.operation_type.as_deref().unwrap(),
                        &browser_operation_payload(&BrowserOperation::ClearHistory)
                            .to_string()
                            .into_bytes(),
                    ),
                    &mut doc,
                    &root,
                )
                .expect_err("resource history clear must not complete without durable storage");
            assert!(error.contains("Previous history was kept"));
            assert_eq!(w.history.len(), 1);
        }
    }

    /// Bookmark removal and row control identities.
    #[test]
    fn bookmark_rows_are_reversible_uniquely_addressed_actions() {
        // Phase `removing_one_bookmark_is_an_ordinary_reversible_document_action`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.bookmarks.add("https://test.example", "Test bookmark");
            let (doc, _root) = synced(&mut w);
            let resources = ui::resources::resources(&w, &doc);
            let bookmarks = resources[0]
                .children
                .iter()
                .find(|r| r.id.0 == "browser.bookmarks")
                .unwrap();
            let remove = bookmarks.children[0]
                .actions
                .iter()
                .find(|a| a.id == "remove")
                .unwrap();
            assert_eq!(
                remove.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::None
            );
            assert_eq!(remove.kind, OperationKind::Document);
        }

        // Phase `bookmark_row_controls_use_unique_normalized_action_identities`.
        {
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
            let controls = ui::widget::controls::control_action_bindings(&w)
                .into_iter()
                .filter_map(|binding| match binding.kind {
                    ui::widget::ControlBindingKind::Action(address)
                        if address.action == "remove" =>
                    {
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
                    control_id == &format!("{resource_id}.remove")
                        && address.resource == resource_id
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
                vec!["show", "open", "reveal"]
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
                schema_version: 1,
                bytes: br#"{"url":"https://normal.test"}"#.to_vec(),
            };
            assert!(!widget.restore_continuation_metadata(
                &metadata,
                &mut document,
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
        widget.sync(&mut document, &automerge::ROOT);
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
    pub(crate) fn clear_history_on_workspace_exit(&mut self) {
        if !self.headless
            && self.browsing_mode.persists_personal_state()
            && self.settings.clear_on_exit
        {
            self.history.clear();
            if let Err(error) = self.history.save() {
                tracing::error!(%error,"Could not clear browser history on exit");
            }
        }
    }
}
