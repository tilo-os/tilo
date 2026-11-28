//! The words an app's action uses with the page its window shows, as JSON.
//!
//! An app's actions run in another process from the window that holds the engine, so a page
//! action travels as one message and one answer (`ToUi::Page`/`ToApp::PageAnswer`). This is
//! what those messages say: a request names an operation and its arguments, and an answer is
//! the page's reply as JSON. Nothing here is UI or a second page model: every operation is the
//! engine's own ([`PageRequest`] or a [`WebViewCommand`]), and the answer is the engine's
//! [`PageReply`] or [`PageFailure`] as it came.
//!
//! A request is an object whose `op` names the operation:
//!
//! ```json
//! {"op": "outline", "from": 12, "frame": "3"}
//! {"op": "click", "element": "d1e5"}
//! {"op": "navigate", "url": "https://example.org/"}
//! ```
//!
//! An answer to a page operation is the engine's own:
//!
//! ```json
//! {"ok": true, "url": "https://example.org/", "message": "…", "outline": "…",
//!  "data": … , "navigation": "none", "loading": false,
//!  "record": {"seq": 4, "others": [], "others_earlier": 0}}
//! ```
//!
//! and a refusal says `{"ok": false, "code": "…", "message": "…", "record": …}`. An operation
//! that does not answer (a navigation, a zoom) answers `{"ok": true}` at once; what became of
//! the page reaches its view as the page's own events.

use base64::Engine as _;
use tilo_webview::{
    AddressFields, CredentialAction, CredentialForm, FrameId, KeyModifiers, NavigationDirection,
    NavigationEntry, PageFailure, PageReply, PageRequest, PageResourceNote, PointerButton,
    PointerPoint, PointerStep, PointerStepKind, ScrollTarget, WebViewCommand,
};

use crate::Asks;

/// How long a settle waits for the page to go quiet, and at most, by default.
const SETTLE_QUIET_MS: u32 = 500;
const SETTLE_TIMEOUT_MS: u32 = 10_000;
/// The steps of a drag, so a gesture is a bounded amount of work whoever asks.
const DRAG_STEPS: u32 = 8;
/// The longest path an extension package is named by.
const MAX_EXTENSION_PATH: usize = 4096;
/// The address a file's bytes stand at in this device's content store.
const BLOB_SCHEME: &str = "tilo-blob://";
/// The longest identifier an extension is named by.
const MAX_EXTENSION_ID: usize = 128;
/// The longest text an extension's keyword takes after it, as the engine carries it.
const MAX_OMNIBOX_TEXT: usize = 2048;

/// What an app asked the page to do.
#[derive(Debug, Clone)]
pub enum Ask {
    /// The engine's own page request: reading and operating the page.
    Page(PageRequest),
    /// The engine's own command: navigating, finding, zooming, answering a question.
    Command(WebViewCommand),
    /// What the page waits on and last reported: its own state, which asks the page nothing.
    Asks,
    /// Something about the page's logins: the engine's forms, or the vault's logins for the
    /// page's site ([`crate::PageSession::login`]). `origin` is the app's word for the site,
    /// which only starts the question of where the page is.
    Login {
        origin: String,
        ask: crate::LoginAsk,
    },
    /// The page is private: its window's engine is started on a fresh, disposable profile,
    /// which holds nothing the person signed in to and goes when the window does. Asked before
    /// the page says anything else, since it settles which engine the page has.
    Private,
    /// The page's profile opens signed in to the sites the person brought their sign-ins of from
    /// a browser of theirs ([`crate::PageSession::open_sign_ins`]). A private page opens none.
    SignIns,
    /// Install the extension package the person chose, named by the address its bytes stand at
    /// in this device's content store (`tilo-blob://…`). A sandboxed app never names a host
    /// path, so the window reads the bytes and hands the engine the package.
    ExtensionInstall { file: String },
    /// What the person typed in the page's address: when it starts with an extension's keyword
    /// and a space, what follows is the extension's, which hears of it.
    AddressTyped {
        /// All the address holds.
        text: String,
        /// The extension whose keyword it starts with, when it starts with one.
        extension: Option<String>,
        /// What that extension is called.
        name: String,
        /// What follows its keyword.
        rest: String,
    },
    /// The person pressed Enter, or chose a row: what the extension is given.
    AddressEntered { text: String },
    /// The person left the address without Enter: the extension hears of it.
    AddressLeft,
    /// Close the page the extension showed beside this one.
    PanelClose,
    /// Ask the page again what a request for `url` carries, for the download the person
    /// retries, whose job ended as `job`.
    DownloadAgain { url: String, job: String },
}

/// Read a request an app's action wrote.
///
/// One arm per operation, which reads best as the one table this closed vocabulary is; a table
/// split into helpers per operation would only move the same lines apart.
///
/// # Errors
/// The text is no JSON object, or it names no operation this vocabulary has.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per operation of one closed vocabulary"
)]
pub fn decode(request: &str) -> Result<Ask, String> {
    let value: serde_json::Value = serde_json::from_str(request)
        .map_err(|error| format!("a page request is no object: {error}"))?;
    let op = value
        .get("op")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "a page request names no op".to_owned())?;
    Ok(match op {
        "outline" => Ask::Page(PageRequest::Outline {
            from: integer(&value, "from"),
            frame: frame(&value),
        }),
        "click" => Ask::Page(PageRequest::Click {
            element: element(&value, "element")?,
            frame: frame(&value),
        }),
        "hover" => Ask::Page(PageRequest::Hover {
            element: element_opt(&value, "element"),
            point: point(&value),
        }),
        "drag" => Ask::Page(PageRequest::Drag {
            from: element_opt(&value, "drag_from"),
            from_point: point_named(&value, "from_x", "from_y"),
            to: element_opt(&value, "to"),
            to_point: point_named(&value, "to_x", "to_y"),
            steps: u8::try_from(integer(&value, "drag_steps").unwrap_or(DRAG_STEPS))
                .unwrap_or(u8::MAX),
        }),
        "path" => Ask::Page(PageRequest::Path {
            element: element_opt(&value, "element"),
            points: points(&value, "points"),
        }),
        "fill" => Ask::Page(PageRequest::Fill {
            element: element(&value, "element")?,
            text: text(&value, "text").unwrap_or_default(),
            secret: flag(&value, "secret"),
            frame: frame(&value),
        }),
        "select" => Ask::Page(PageRequest::Select {
            element: element(&value, "element")?,
            values: texts(&value, "values"),
            frame: frame(&value),
        }),
        "fill_files" => Ask::Page(PageRequest::FillFiles {
            element: element(&value, "element")?,
            paths: texts(&value, "paths"),
            frame: frame(&value),
        }),
        "press" => {
            let chord = text(&value, "key").unwrap_or_default();
            Ask::Page(PageRequest::Press {
                element: element_opt(&value, "element"),
                key: tilo_webview::PageKey::parse(&chord).unwrap_or_else(|| {
                    tilo_webview::PageKey {
                        key: chord,
                        modifiers: KeyModifiers::default(),
                    }
                }),
                frame: frame(&value),
            })
        }
        "submit" => Ask::Page(PageRequest::Submit {
            element: element(&value, "element")?,
            cross_origin: flag(&value, "cross_origin"),
            frame: frame(&value),
        }),
        "changes" => Ask::Page(PageRequest::Changes {
            frame: frame(&value),
        }),
        "at" => Ask::Page(PageRequest::At {
            x: number(&value, "x").unwrap_or_default(),
            y: number(&value, "y").unwrap_or_default(),
            frame: frame(&value),
        }),
        "settle" => Ask::Page(PageRequest::Settle {
            quiet_ms: integer(&value, "quiet_ms").unwrap_or(SETTLE_QUIET_MS),
            timeout_ms: integer(&value, "timeout_ms").unwrap_or(SETTLE_TIMEOUT_MS),
            text: words(&value, "text"),
            gone: words(&value, "gone"),
            frame: frame(&value),
        }),
        "console" => Ask::Page(PageRequest::Console {
            since: u64::from(integer(&value, "since").unwrap_or(0)),
        }),
        "network" => Ask::Page(PageRequest::Network {
            since: u64::from(integer(&value, "since").unwrap_or(0)),
        }),
        "log" => Ask::Page(PageRequest::Log {
            since: u64::from(integer(&value, "since").unwrap_or(0)),
        }),
        "fields" => Ask::Page(PageRequest::Fields {
            frame: frame(&value),
        }),
        "forms" => Ask::Page(PageRequest::Forms {
            frame: frame(&value),
        }),
        "search" => Ask::Page(PageRequest::Search {
            text: text(&value, "text").unwrap_or_default(),
            frame: frame(&value),
            frames: flag(&value, "frames"),
        }),
        "table" => Ask::Page(PageRequest::Table {
            element: element_opt(&value, "element"),
            from: integer(&value, "from"),
            frame: frame(&value),
        }),
        "fill_form" => Ask::Page(PageRequest::FillForm {
            form: text(&value, "form").unwrap_or_default(),
            answers: answers(&value),
            submit: match (
                text(&value, "submit_button"),
                value.get("submit").and_then(serde_json::Value::as_bool),
            ) {
                (Some(button), _) => tilo_webview::FormSubmit::Button(button),
                (None, Some(true)) => tilo_webview::FormSubmit::First,
                (None, _) => tilo_webview::FormSubmit::No,
            },
            cross_origin: flag(&value, "cross_origin"),
            frame: frame(&value),
        }),
        "reader" => Ask::Page(PageRequest::Reader {
            enabled: flag(&value, "enabled"),
        }),
        "capture" => Ask::Page(PageRequest::Capture {
            element: element_opt(&value, "element"),
            max_width: integer(&value, "max_width"),
            frame: frame(&value),
        }),
        "find" => Ask::Page(PageRequest::Find {
            role: words(&value, "role"),
            name: words(&value, "name"),
            nth: integer(&value, "nth"),
            frame: frame(&value),
            frames: flag(&value, "frames"),
        }),
        "pointer" => Ask::Page(PageRequest::Pointer {
            steps: texts(&value, "steps")
                .iter()
                .filter_map(|step| pointer_step(step))
                .collect(),
            button: PointerButton::parse(text(&value, "button").as_deref().unwrap_or("left")),
        }),
        "scroll" => Ask::Page(PageRequest::Scroll {
            element: element_opt(&value, "element"),
            to: text(&value, "to").as_deref().and_then(ScrollTarget::parse),
            by: value
                .get("by")
                .and_then(serde_json::Value::as_i64)
                .and_then(|by| i32::try_from(by).ok()),
            into_view: element_opt(&value, "into_view"),
            frame: frame(&value),
        }),
        // What the engine does rather than the page: navigation, the find bar, zoom, and the
        // answers a person gives the page's own questions.
        "navigate" => Ask::Command(WebViewCommand::Navigate(
            text(&value, "url").ok_or_else(|| "navigate names no address".to_owned())?,
        )),
        "back" => Ask::Command(WebViewCommand::GoBack),
        "forward" => Ask::Command(WebViewCommand::GoForward),
        "reload" => Ask::Command(WebViewCommand::Reload),
        "stop" => Ask::Command(WebViewCommand::StopLoading),
        "exit_fullscreen" => Ask::Command(WebViewCommand::ExitFullscreen),
        "zoom" => Ask::Command(WebViewCommand::SetZoomLevel(
            number(&value, "level").unwrap_or_default(),
        )),
        "find_in_page" => Ask::Command(WebViewCommand::Find {
            id: integer(&value, "id").unwrap_or(1),
            text: text(&value, "text").unwrap_or_default(),
            forward: value
                .get("forward")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            next: flag(&value, "next"),
        }),
        "stop_finding" => Ask::Command(WebViewCommand::StopFinding),
        "answer_dialog" => Ask::Command(WebViewCommand::AnswerPageDialog {
            id: integer(&value, "id").unwrap_or_default(),
            accept: flag(&value, "accept"),
            text: text(&value, "text"),
        }),
        "answer_sign_in" => Ask::Command(WebViewCommand::AnswerHttpAuth {
            id: integer(&value, "id").unwrap_or_default(),
            asker: text(&value, "asker").unwrap_or_default(),
            credentials: match (text(&value, "username"), text(&value, "password")) {
                (Some(username), Some(password)) => {
                    Some(tilo_webview::HttpAuthCredentials::new(username, password))
                }
                _ => None,
            },
        }),
        "answer_held_request" => Ask::Command(WebViewCommand::AnswerDeparture {
            id: integer(&value, "id").unwrap_or_default(),
            leaving: if flag(&value, "allow") {
                tilo_webview::Leaving::Allowed
            } else {
                tilo_webview::Leaving::Kept
            },
        }),
        "answer_permission" => Ask::Command(WebViewCommand::PermissionDecision {
            id: integer(&value, "id").unwrap_or_default(),
            allow: flag(&value, "allow"),
        }),
        "answer_extension" => Ask::Command(WebViewCommand::ExtensionPermissionDecision {
            request: integer(&value, "id").unwrap_or_default(),
            allow: flag(&value, "allow"),
        }),
        "dismiss_permission" => Ask::Command(WebViewCommand::PermissionDismissed {
            id: integer(&value, "id").unwrap_or_default(),
        }),
        "context_menu_action" => Ask::Command(WebViewCommand::ContextMenuAction {
            id: integer(&value, "id").unwrap_or_default(),
            command: integer(&value, "command").unwrap_or_default(),
        }),
        "asks" => Ask::Asks,
        // A private page's window starts its engine on a profile of its own.
        "private" => Ask::Private,
        // A page of the person's own opens signed in where they brought their sign-ins.
        "sign_ins" => Ask::SignIns,
        // The engine's own developer tools, opened beside the page.
        "developer_tools" => Ask::Command(WebViewCommand::ShowDeveloperTools),
        // The page written as a PDF the person can print: where the engine wrote it reaches
        // the app as the window's note, once the engine has finished writing it.
        "print" => Ask::Command(WebViewCommand::PrintToPdf { request_id: 1 }),
        // The page's sound, and where it can go back or forward to: the engine answers the
        // history through `asks` once it has read it.
        "mute" => Ask::Command(WebViewCommand::SetAudioMuted(flag(&value, "muted"))),
        "history" => Ask::Command(WebViewCommand::RequestNavigationHistory {
            request_id: 1,
            direction: if text(&value, "direction").as_deref() == Some("forward") {
                NavigationDirection::Forward
            } else {
                NavigationDirection::Back
            },
        }),
        "history_entry" => Ask::Command(WebViewCommand::NavigateToHistoryEntry {
            request_id: 1,
            id: integer(&value, "entry").unwrap_or_default(),
        }),
        // What a site's connection, permissions and stored data are, and what the person may do
        // about them: the engine answers the status through `asks` and the result as an answer.
        "site_status" => Ask::Command(WebViewCommand::RequestSiteStatus {
            request_id: 1,
            origin: text(&value, "origin").unwrap_or_default(),
        }),
        "site_action" => Ask::Command(WebViewCommand::SiteAction {
            request_id: 1,
            origin: text(&value, "origin").unwrap_or_default(),
            generation: u64::from(integer(&value, "generation").unwrap_or_default()),
            action: text(&value, "doing").unwrap_or_default(),
        }),
        "site_exceptions" => {
            Ask::Command(WebViewCommand::SetSiteExceptions(texts(&value, "origins")))
        }
        // The extensions this device keeps: what is installed, one installed from the package
        // the person chose (a CRX3 package or a plain zip), one installed from the Chrome Web
        // Store, the store asked for newer versions of them, one turned on or off, and one
        // forgotten. The engine unpacks and launches them; the app never reads a package.
        "extension_list" => Ask::Command(WebViewCommand::ExtensionList { request_id: 1 }),
        "extension_install" => {
            // The chosen file's bytes stand in this device's content store; the window reads
            // them and hands the engine the package.
            let file = text(&value, "file").unwrap_or_default();
            if !file.starts_with(BLOB_SCHEME) || file.len() > MAX_EXTENSION_PATH {
                return Err("an extension package is named by its content's address".to_owned());
            }
            Ask::ExtensionInstall { file }
        }
        "extension_install_store" => {
            let item = text(&value, "item").unwrap_or_default();
            if item.trim().is_empty() || item.len() > MAX_EXTENSION_PATH {
                return Err("a store item is named by its address or its id".to_owned());
            }
            Ask::Command(WebViewCommand::ExtensionInstallStore {
                request_id: 1,
                item: item.trim().to_owned(),
            })
        }
        "extension_update_check" => {
            Ask::Command(WebViewCommand::ExtensionUpdateCheck { request_id: 1 })
        }
        "extension_enable" => Ask::Command(WebViewCommand::ExtensionEnable {
            request_id: 1,
            id: extension_id(&value)?,
            enabled: flag(&value, "enabled"),
        }),
        "extension_remove" => Ask::Command(WebViewCommand::ExtensionRemove {
            request_id: 1,
            id: extension_id(&value)?,
        }),
        // What the person did with a notification an extension showed: pressed one of its
        // buttons, or took it away.
        "extension_notice_chosen" => Ask::Command(WebViewCommand::ExtensionNotificationChosen {
            id: extension_id(&value)?,
            notification: text(&value, "notification").unwrap_or_default(),
            button: text(&value, "button").and_then(|button| button.parse::<u8>().ok()),
        }),
        // A press on an extension's toolbar button: the engine runs the extension's action, or
        // opens the page it offers.
        "extension_click" => Ask::Command(WebViewCommand::ExtensionClick {
            id: extension_id(&value)?,
        }),
        // The person asked for the extension's side panel beside the page: its worker knows
        // which page that is, and says what the panel shows.
        "open_side_panel" => Ask::Command(WebViewCommand::ExtensionOpenSidePanel {
            id: extension_id(&value)?,
        }),
        // What the person types in the address: an extension's keyword and what follows it, the
        // Enter or row they chose, and their leaving it. The engine tells the extension of each
        // and lists what it suggests.
        "address_typed" => {
            let typed = bounded_text(&value, "text", MAX_OMNIBOX_TEXT)?;
            let extension = text(&value, "extension");
            if extension
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > MAX_EXTENSION_ID)
            {
                return Err("an extension is named by its identifier".to_owned());
            }
            Ask::AddressTyped {
                text: typed,
                extension,
                name: bounded_text(&value, "name", MAX_OMNIBOX_TEXT)?,
                rest: bounded_text(&value, "rest", MAX_OMNIBOX_TEXT)?,
            }
        }
        "address_entered" => Ask::AddressEntered {
            text: bounded_text(&value, "text", MAX_OMNIBOX_TEXT)?,
        },
        "address_left" => Ask::AddressLeft,
        // The window's second page goes: the extension's side panel, or the page it asked the
        // person to sign in on.
        "panel_close" => Ask::PanelClose,
        // A download the person retries: what a request for its address carries from this page
        // now, kept for the new job the app's call starts.
        "download_again" => Ask::DownloadAgain {
            url: text(&value, "url")
                .ok_or_else(|| "a retried download names its address".to_owned())?,
            job: text(&value, "job").unwrap_or_default(),
        },
        // The page's logins: the forms the engine finds and the capture it takes of one the
        // person typed, and the logins the workspace's vault keeps for the page's site. The
        // password never crosses this vocabulary.
        "credential" => Ask::Login {
            origin: text(&value, "origin").unwrap_or_default(),
            ask: credential(&value)?,
        },
        // The person's answer to a download the page offered: a saved one is handed to the
        // app, which fetches it as a job of its own.
        "answer_download" => Ask::Command(WebViewCommand::AnswerDownload {
            id: integer(&value, "id").unwrap_or_default(),
            leaving: if flag(&value, "allow") {
                tilo_webview::Leaving::Allowed
            } else {
                tilo_webview::Leaving::Kept
            },
        }),
        other => return Err(format!("a page request names no operation {other:?}")),
    })
}

/// The answer an operation that does not wait for the page gives at once.
#[must_use]
pub fn accepted() -> String {
    serde_json::json!({ "ok": true }).to_string()
}

/// A page operation's answer, as an app's action reads it.
#[must_use]
pub fn encode(result: &Result<PageReply, PageFailure>) -> String {
    match result {
        Ok(reply) => {
            let mut answer = serde_json::json!({
                "ok": true,
                "url": &reply.url,
                "message": &reply.message,
                "outline": &reply.outline,
                "data": &reply.data,
                "navigation": reply.navigation.as_str(),
                "loading": reply.loading,
                "record": record(&reply.record),
            });
            if let Some(image) = &reply.image
                && let Some(fields) = answer.as_object_mut()
            {
                fields.insert(
                    "image".to_owned(),
                    base64::engine::general_purpose::STANDARD
                        .encode(image)
                        .into(),
                );
            }
            answer.to_string()
        }
        Err(failure) => serde_json::json!({
            "ok": false,
            "code": &failure.code,
            "message": &failure.message,
            "record": record(&failure.record),
        })
        .to_string(),
    }
}

/// What the page waits on and last reported, as an app's action reads it.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the asks are written field by field, in the order the app reads them"
)]
pub fn asks_json(asks: &Asks) -> String {
    let dialog = asks.dialog.as_ref().map(|dialog| {
        serde_json::json!({
            "id": dialog.id,
            "kind": match &dialog.kind {
                tilo_webview::PageDialogKind::Alert => "alert",
                tilo_webview::PageDialogKind::Confirm => "confirm",
                tilo_webview::PageDialogKind::Prompt => "prompt",
                tilo_webview::PageDialogKind::Leave { .. } => "leave",
            },
            "origin": &dialog.origin,
            "message": &dialog.message,
            "default_text": &dialog.default_text,
        })
    });
    let sign_in = asks.sign_in.as_ref().map(|ask| {
        serde_json::json!({
            "id": ask.id,
            "asker": &ask.asker,
            "site": ask.site(),
            "proxy": ask.proxy,
            "realm": &ask.realm,
            "scheme": &ask.scheme,
            "retry": ask.retry,
        })
    });
    let permission = asks.permission.as_ref().map(|(id, origin, rights)| {
        serde_json::json!({"id": id, "origin": origin, "permissions": rights})
    });
    let held: Vec<serde_json::Value> = asks
        .held
        .iter()
        .map(|held| {
            serde_json::json!({
                "id": held.id,
                "kind": held.kind.as_str(),
                "method": &held.method,
                "origin": &held.origin,
                "has_picture": held.capture.is_some(),
            })
        })
        .collect();
    let menu = asks.menu.as_ref().map(|menu| {
        serde_json::json!({
            "id": menu.id,
            "x": menu.x,
            "y": menu.y,
            "link": &menu.link,
            "items": menu_json(&menu.items),
        })
    });
    let find = asks
        .find
        .map(|(active, count)| serde_json::json!({"active": active, "count": count}));
    serde_json::json!({
        "dialog": dialog,
        "sign_in": sign_in,
        "permission": permission,
        "held": held,
        "menu": menu,
        "find": find,
        "zoom": asks.zoom,
        "fullscreen": asks.fullscreen,
        "progress": asks.progress,
        "title": &asks.title,
        "loading": asks.loading,
        "muted": asks.muted,
        "media": asks.media.as_ref().map(|media| {
            serde_json::json!({
                "element": &media.element_id,
                "viewport": media.viewport_rect,
                "intrinsic": media.intrinsic_size,
                "label": &media.label,
            })
        }),
        "back": entries_json(&asks.back),
        "forward": entries_json(&asks.forward),
        "site": asks.site.as_ref().map(site_json),
        "extensions": asks
            .extensions
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "name": entry.name,
                    "version": entry.version,
                    "manifest_version": entry.manifest_version,
                    "description": entry.description,
                    "permissions": entry.permissions,
                    "enabled": entry.enabled,
                    "keyword": entry.keyword,
                    "side_panel": entry.side_panel,
                })
            })
            .collect::<Vec<_>>(),
        "extension_buttons": asks
            .extension_buttons
            .iter()
            .map(|button| {
                serde_json::json!({
                    "id": button.id,
                    "title": button.title,
                    "badge": button.badge,
                    "enabled": button.enabled,
                    "popup": button.popup,
                })
            })
            .collect::<Vec<_>>(),
        "extension_notices": asks
            .extension_notices
            .iter()
            .map(|notice| {
                serde_json::json!({
                    "id": notice.id,
                    "notification": notice.notification,
                    "title": notice.title,
                    "message": notice.message,
                    "buttons": notice.buttons,
                })
            })
            .collect::<Vec<_>>(),
    })
    .to_string()
}

/// The identifier an extension request names, bounded so no request names a folder of its own.
fn extension_id(value: &serde_json::Value) -> Result<String, String> {
    let id = text(value, "id").unwrap_or_default();
    if id.is_empty() || id.len() > MAX_EXTENSION_ID {
        return Err("an extension is named by its identifier".to_owned());
    }
    Ok(id)
}

/// The field of a login request that names the running call of the app's action that asked for
/// it. The app's runtime writes it, in place of anything the app's own code wrote there, so the
/// vault decides a fill as whoever made that call.
pub const ASKED_BY: &str = "on_behalf_of";

/// The login action a request names, with the fields it takes. A password is never part of one:
/// the engine captures and fills it, and the vault keeps it.
fn credential(value: &serde_json::Value) -> Result<crate::LoginAsk, String> {
    use crate::LoginAsk;
    let field = |name: &str| text(value, name).unwrap_or_default();
    Ok(match field("credential").as_str() {
        "status" => LoginAsk::Engine(CredentialAction::Status),
        "cancel" => LoginAsk::Engine(CredentialAction::Cancel),
        "discover" => LoginAsk::Engine(CredentialAction::Discover),
        "addresses" => LoginAsk::Engine(CredentialAction::DiscoverAddresses),
        "capture" => LoginAsk::Engine(CredentialAction::Capture {
            form: field("form"),
        }),
        "fill_address" => LoginAsk::Engine(CredentialAction::FillAddress {
            form: field("form"),
            profile: address_fields(value)?,
        }),
        "list" => LoginAsk::List,
        "save" => LoginAsk::Save {
            capture: field("capture"),
        },
        "update" => LoginAsk::Update {
            capture: field("capture"),
            item: field("account"),
        },
        "fill" => LoginAsk::Fill {
            form: field("form"),
            item: field("account"),
            asked_by: text(value, ASKED_BY),
        },
        // A field filled from a login the vault keeps: the request names the field and the
        // login, never what it keeps.
        "fill_field" => LoginAsk::FillField {
            element: element(value, "element")?,
            item: field("account"),
            asked_by: text(value, ASKED_BY),
        },
        "delete" => LoginAsk::Delete {
            item: field("account"),
        },
        other => return Err(format!("a credential request names no action {other:?}")),
    })
}

/// The fields of an address a form is filled with, as the app's own address holds them.
fn address_fields(value: &serde_json::Value) -> Result<AddressFields, String> {
    let Some(address) = value.get("address") else {
        return Err("a fill-address request names no address".to_owned());
    };
    let field = |name: &str| {
        address
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(AddressFields {
        full_name: field("full_name"),
        given_name: field("given_name"),
        family_name: field("family_name"),
        street: field("street"),
        city: field("city"),
        region: field("region"),
        postal_code: field("postal_code"),
        country: field("country"),
    })
}

/// What a login action came to, for the window to hand the app as its call: how it ended, the
/// forms the engine found, the vault's logins for the site, the capture the engine took, and the
/// name it read.
#[must_use]
pub fn credential_note(
    action: &str,
    success: bool,
    message: &str,
    forms: &[CredentialForm],
    accounts: &[crate::VaultLogin],
    capture: Option<&str>,
    username: Option<&str>,
    late: bool,
) -> String {
    let forms: Vec<serde_json::Value> = forms
        .iter()
        .map(|form| {
            serde_json::json!({
                "form": form.id,
                "username": form.username,
                "kind": form.kind,
            })
        })
        .collect();
    let accounts: Vec<serde_json::Value> = accounts
        .iter()
        .map(|account| {
            serde_json::json!({
                "account": account.item,
                "username": account.username,
                "title": account.title,
                "own": account.own,
            })
        })
        .collect();
    serde_json::json!({
        "note": "credential",
        "did": action,
        "success": success,
        "message": message,
        "forms": serde_json::Value::Array(forms).to_string(),
        "accounts": serde_json::Value::Array(accounts).to_string(),
        "capture": capture.unwrap_or_default(),
        "username": username.unwrap_or_default(),
        "late": late,
    })
    .to_string()
}

/// What this device keeps installed, for the window to hand the app as its call: each extension
/// and whether the person turned it on.
#[must_use]
pub fn extensions_note(installed: &[tilo_webview::ExtensionEntry], message: &str) -> String {
    let entries: Vec<serde_json::Value> = installed
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "name": entry.name,
                "version": entry.version,
                "manifest_version": entry.manifest_version,
                "description": entry.description,
                "permissions": entry.permissions,
                "enabled": entry.enabled,
                "keyword": entry.keyword,
                "side_panel": entry.side_panel,
            })
        })
        .collect();
    serde_json::json!({
        "note": "extensions",
        "installed": serde_json::Value::Array(entries).to_string(),
        "message": message,
    })
    .to_string()
}

/// How an install, a turning on or off, or a removal came out, for the window to hand the app as
/// its call: the engine's kind of failure and its own words, and the extension it was about.
#[must_use]
pub fn extension_outcome_note(ok: bool, kind: &str, message: &str, id: &str) -> String {
    serde_json::json!({
        "note": "extension",
        "ok": ok,
        "kind": kind,
        "message": message,
        "id": id,
    })
    .to_string()
}

/// What asking the store for newer versions came to, for the window to hand the app as its call:
/// how many extensions were asked about, which were updated (and what they replaced), and which
/// could not be.
#[must_use]
pub fn extension_updates_note(
    checked: u32,
    updated: &[tilo_webview::ExtensionUpdate],
    failed: &[String],
) -> String {
    let updated: Vec<serde_json::Value> = updated
        .iter()
        .map(|update| {
            serde_json::json!({
                "id": update.id,
                "name": update.name,
                "from": update.from,
                "version": update.version,
            })
        })
        .collect();
    serde_json::json!({
        "note": "extension_updates",
        "checked": checked,
        "updated": serde_json::Value::Array(updated).to_string(),
        "failed": failed.join("; "),
    })
    .to_string()
}

/// The page written as a PDF, for the window to hand the app as its call: whether it was,
/// where the file is, what it is called, and why not in the engine's own words.
#[must_use]
pub fn print_note(ok: bool, path: &str, name: &str, message: &str) -> String {
    serde_json::json!({
        "note": "print",
        "ok": ok,
        "path": path,
        "name": name,
        "message": message,
    })
    .to_string()
}

/// What an extension's page shown beside the window's page became, for the window to hand the
/// app as its call: which page it is and what it shows, or that it went.
#[must_use]
pub fn panel_note(panel: Option<&crate::PanelInfo>) -> String {
    match panel {
        Some(panel) => serde_json::json!({
            "note": "panel",
            "open": true,
            "kind": &panel.kind,
            "id": &panel.id,
            "url": &panel.url,
            "interactive": panel.interactive,
        })
        .to_string(),
        None => serde_json::json!({
            "note": "panel",
            "open": false,
            "kind": "",
            "id": "",
            "url": "",
            "interactive": false,
        })
        .to_string(),
    }
}

/// What the address offers the extension it addresses, for the window to hand the app as its
/// call: the extension, what follows its keyword, and its rows, or that it no longer addresses
/// one.
#[must_use]
pub fn omnibox_note(state: Option<&crate::omnibox::State>) -> String {
    match state {
        Some(state) => serde_json::json!({
            "note": "omnibox",
            "id": &state.id,
            "name": &state.name,
            "text": &state.text,
            "rows": serde_json::Value::Array(
                state
                    .rows
                    .iter()
                    .map(|row| serde_json::json!({"title": row.title, "content": row.content}))
                    .collect(),
            )
            .to_string(),
        })
        .to_string(),
        None => serde_json::json!({
            "note": "omnibox",
            "id": "",
            "name": "",
            "text": "",
            "rows": "[]",
        })
        .to_string(),
    }
}

/// A download the person saved, for the window to hand the app as its call: where it comes
/// from, the name the page suggested, and the handle the window keeps its request's context
/// under, which the app's download redeems once.
#[must_use]
pub fn download_note(url: &str, name: &str, request: &str, again: Option<&str>) -> String {
    let mut note = serde_json::Map::new();
    note.insert("note".to_owned(), "download".into());
    note.insert("url".to_owned(), url.into());
    note.insert("name".to_owned(), name.into());
    note.insert("request".to_owned(), request.into());
    // A retried download names the job it ended as, which the new one takes the place of.
    if let Some(job) = again {
        note.insert("again".to_owned(), job.into());
    }
    serde_json::Value::Object(note).to_string()
}

/// Where a download an extension started stands, as the engine that saves it tells.
pub struct SeenDownload<'a> {
    pub id: u32,
    pub state: &'a str,
    pub name: &'a str,
    pub url: &'a str,
    pub path: &'a str,
    pub received: u64,
    pub total: u64,
    pub error: &'a str,
}

/// The note a download an extension started becomes, for the Browser's one list of downloads.
#[must_use]
pub fn extension_download_note(seen: &SeenDownload<'_>) -> String {
    serde_json::json!({
        "note": "extension_download",
        "id": seen.id,
        "state": seen.state,
        "name": seen.name,
        "url": seen.url,
        "path": seen.path,
        "received": seen.received,
        "total": seen.total,
        "error": seen.error,
    })
    .to_string()
}

/// A page the page opened in a window of its own (`window.open`, a link that names a new tab),
/// for the window to hand the app as its call.
#[must_use]
pub fn open_note(url: &str, popup: bool) -> String {
    serde_json::json!({"note": "open", "url": url, "popup": popup}).to_string()
}

/// The video the page shows changed (it appeared, went, or the page went elsewhere), for the
/// window to hand the app as its call: the app reads the video from the page's asks and offers
/// to float it, so the person's control follows the page without waiting for another answer.
#[must_use]
pub fn media_note() -> String {
    serde_json::json!({"note": "media"}).to_string()
}

/// Whether the page is in fullscreen, for the window to hand the app as its call: its record
/// keeps it, since the page fills the screen while it is in it and shows the way out.
#[must_use]
pub fn fullscreen_note(on: bool) -> String {
    serde_json::json!({"note": "fullscreen", "fullscreen": on}).to_string()
}

/// What the page's own history holds one way, for the window to hand the app as its call: the
/// direction and the pages, each as its id, address and title.
#[must_use]
pub fn history_note(direction: NavigationDirection, entries: &[NavigationEntry]) -> String {
    serde_json::json!({
        "note": "history",
        "direction": direction.as_str(),
        "entries": serde_json::Value::Array(entries_json(entries)).to_string(),
    })
    .to_string()
}

/// What a site's connection, permissions and stored data are, as the app reads them.
fn site_json(site: &tilo_webview::SiteStatus) -> serde_json::Value {
    let permissions: Vec<serde_json::Value> = site
        .permissions
        .iter()
        .map(|(kind, state)| serde_json::json!({"kind": kind, "state": state}))
        .collect();
    serde_json::json!({
        "origin": site.origin,
        "generation": site.generation,
        "blocked": site.blocked,
        "protected": site.effective,
        "security": site.security,
        "certificate": site.certificate_status,
        "issuer": site.issuer,
        "subject": site.subject,
        "permissions": permissions,
        "can_reset": site.can_reset,
    })
}

/// One direction's pages, each as its id, address and title.
fn entries_json(entries: &[tilo_webview::NavigationEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "url": entry.url,
                "title": entry.title,
            })
        })
        .collect()
}

/// What the page started or stopped waiting on a person's yes for, as the window tells the app
/// (`ToApp::PagePrompt`):
///
/// ```json
/// {"raised": "permission", "id": 3, "on": "https://meet.example",
///  "facts": {"origin": "https://meet.example", "wants": "camera"}}
/// {"ended": "permission", "id": 3}
/// ```
#[must_use]
pub fn prompt_json(change: &crate::PromptChange) -> String {
    match change {
        crate::PromptChange::Raised(prompt) => {
            let facts: serde_json::Map<String, serde_json::Value> = prompt
                .facts
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone().into()))
                .collect();
            serde_json::json!({
                "raised": prompt.key.kind.as_str(),
                "id": prompt.key.id,
                "on": &prompt.on,
                "facts": facts,
            })
        }
        crate::PromptChange::Ended(key) => {
            serde_json::json!({"ended": key.kind.as_str(), "id": key.id})
        }
    }
    .to_string()
}

/// A context menu's rows, nested as the engine offered them.
fn menu_json(items: &[tilo_webview::ContextMenuItem]) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|item| match item {
            tilo_webview::ContextMenuItem::Command {
                id,
                label,
                checked,
                enabled,
            } => serde_json::json!({
                "command": id,
                "label": label,
                "checked": checked,
                "enabled": enabled,
            }),
            tilo_webview::ContextMenuItem::Submenu {
                label,
                enabled,
                items,
            } => serde_json::json!({
                "label": label,
                "enabled": enabled,
                "items": menu_json(items),
            }),
            tilo_webview::ContextMenuItem::Separator => serde_json::json!({"separator": true}),
        })
        .collect()
}

/// Why a request could not be made, in the shape an answer has.
#[must_use]
pub fn refused(why: &str) -> String {
    serde_json::json!({ "ok": false, "code": "refused", "message": why }).to_string()
}

fn record(note: &PageResourceNote) -> serde_json::Value {
    serde_json::json!({
        "seq": note.seq,
        "others": &note.others,
        "others_earlier": note.others_earlier,
    })
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// A text a request names, within what a page carries: what a person types and an engine reads
/// stays bounded, whatever a caller wrote.
fn bounded_text(value: &serde_json::Value, key: &str, most: usize) -> Result<String, String> {
    let held = text(value, key).unwrap_or_default();
    if held.len() > most {
        return Err(format!("a request's {key} is longer than a page carries"));
    }
    Ok(held)
}

/// Words a wait or a find looks for: a blank is the same as leaving them out.
fn words(value: &serde_json::Value, key: &str) -> Option<String> {
    text(value, key).filter(|words| !words.trim().is_empty())
}

fn texts(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn number(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn integer(value: &serde_json::Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

fn flag(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn frame(value: &serde_json::Value) -> Option<FrameId> {
    text(value, "frame").and_then(|frame| FrameId::parse(&frame))
}

fn element(value: &serde_json::Value, key: &str) -> Result<String, String> {
    text(value, key)
        .filter(|element| !element.is_empty())
        .ok_or_else(|| format!("this page operation needs an {key}"))
}

fn element_opt(value: &serde_json::Value, key: &str) -> Option<String> {
    text(value, key).filter(|element| !element.is_empty())
}

fn point(value: &serde_json::Value) -> Option<(f64, f64)> {
    Some((number(value, "x")?, number(value, "y")?))
}

fn point_named(value: &serde_json::Value, x: &str, y: &str) -> Option<(f64, f64)> {
    Some((number(value, x)?, number(value, y)?))
}

fn points(value: &serde_json::Value, key: &str) -> Vec<(f64, f64)> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|point| {
                    let pair = point.as_array()?;
                    Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn answers(value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value
        .get("answers")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// One `kind=move x=10 y=10 element=d1e4 frame=3` step of a pointer gesture; nothing when a
/// field is missing or unreadable.
fn pointer_step(text: &str) -> Option<PointerStep> {
    let mut kind = None;
    let mut element = None;
    let mut frame = None;
    let mut x = None;
    let mut y = None;
    for field in text.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        match name {
            "kind" => kind = Some(value),
            "element" if !value.is_empty() => element = Some(value.to_string()),
            "frame" if !value.is_empty() => frame = Some(FrameId::parse(value)?),
            "x" => x = value.parse::<f64>().ok(),
            "y" => y = value.parse::<f64>().ok(),
            _ => return None,
        }
    }
    let kind = match kind? {
        "move" => PointerStepKind::Move,
        "down" => PointerStepKind::Down,
        "up" => PointerStepKind::Up,
        _ => return None,
    };
    let (x, y) = (x?, y?);
    let at = if element.is_some() {
        PointerPoint::InElement { x, y }
    } else {
        PointerPoint::Viewport { x, y }
    };
    Some(PointerStep {
        kind,
        at,
        element,
        frame,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outline_request_reads_its_arguments() {
        let Ask::Page(PageRequest::Outline { from, frame }) =
            decode(r#"{"op": "outline", "from": 12, "frame": "[frame=3]"}"#).unwrap()
        else {
            panic!("an outline request");
        };
        assert_eq!(from, Some(12));
        assert_eq!(frame.map(|frame| frame.to_string()), Some("3".to_owned()));
    }

    #[test]
    fn an_extension_package_is_named_by_its_content_and_a_host_path_is_refused() {
        let Ask::ExtensionInstall { file } =
            decode(r#"{"op": "extension_install", "file": "tilo-blob://aaaa"}"#).unwrap()
        else {
            panic!("a chosen package");
        };
        assert_eq!(file, "tilo-blob://aaaa");
        assert!(
            decode(r#"{"op": "extension_install", "file": "/home/ada/reader.crx"}"#).is_err(),
            "a sandboxed app names no host path"
        );
        assert!(decode(r#"{"op": "extension_install"}"#).is_err());

        // The store's item is its page's address or its id, and the store is asked for updates.
        let Ask::Command(WebViewCommand::ExtensionInstallStore { item, .. }) = decode(
            r#"{"op": "extension_install_store", "item": "https://chromewebstore.google.com/detail/x/abcdefghijklmnopabcdefghijklmnop"}"#,
        )
        .unwrap()
        else {
            panic!("a store item");
        };
        assert!(item.ends_with("abcdefghijklmnopabcdefghijklmnop"));
        assert!(decode(r#"{"op": "extension_install_store", "item": " "}"#).is_err());
        assert!(matches!(
            decode(r#"{"op": "extension_update_check"}"#).unwrap(),
            Ask::Command(WebViewCommand::ExtensionUpdateCheck { .. })
        ));
    }

    #[test]
    fn a_request_naming_no_operation_is_refused() {
        assert!(decode(r#"{"op": "teleport"}"#).is_err());
        assert!(decode("not json").is_err());
    }

    #[test]
    fn an_answer_says_what_the_page_did() {
        let note = PageResourceNote::default();
        let reply = PageReply {
            url: "https://example.org/".to_owned(),
            message: "read".to_owned(),
            outline: Some("1 heading".to_owned()),
            data: None,
            image: None,
            navigation: tilo_webview::PageNavigation::None,
            loading: false,
            record: note,
        };
        let text = encode(&Ok(reply));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["url"], "https://example.org/");
        assert_eq!(value["outline"], "1 heading");

        let failed = encode(&Err(PageFailure::new("no-element", "no such element")));
        let value: serde_json::Value = serde_json::from_str(&failed).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "no-element");
    }
}
