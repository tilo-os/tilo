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
    FrameId, KeyModifiers, PageFailure, PageReply, PageRequest, PageResourceNote, PointerButton,
    PointerPoint, PointerStep, PointerStepKind, ScrollTarget, WebViewCommand,
};

use crate::Asks;

/// How long a settle waits for the page to go quiet, and at most, by default.
const SETTLE_QUIET_MS: u32 = 500;
const SETTLE_TIMEOUT_MS: u32 = 10_000;
/// The steps of a drag, so a gesture is a bounded amount of work whoever asks.
const DRAG_STEPS: u8 = 8;

/// What an app asked the page to do.
#[derive(Debug, Clone)]
pub enum Ask {
    /// The engine's own page request: reading and operating the page.
    Page(PageRequest),
    /// The engine's own command: navigating, finding, zooming, answering a question.
    Command(WebViewCommand),
    /// What the page waits on and last reported: its own state, which asks the page nothing.
    Asks,
}

/// Read a request an app's action wrote.
///
/// # Errors
/// The text is no JSON object, or it names no operation this vocabulary has.
pub fn decode(request: &str) -> Result<Ask, String> {
    let value: serde_json::Value =
        serde_json::from_str(request).map_err(|error| format!("a page request is no object: {error}"))?;
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
            steps: u8::try_from(integer(&value, "drag_steps").unwrap_or(u32::from(DRAG_STEPS)))
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
                key: tilo_webview::PageKey::parse(&chord).unwrap_or_else(|| tilo_webview::PageKey {
                    key: chord,
                    modifiers: KeyModifiers::default(),
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
                (Some(username), Some(password)) => Some(
                    tilo_webview::HttpAuthCredentials::new(username, password),
                ),
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
        "dismiss_permission" => Ask::Command(WebViewCommand::PermissionDismissed {
            id: integer(&value, "id").unwrap_or_default(),
        }),
        "context_menu_action" => Ask::Command(WebViewCommand::ContextMenuAction {
            id: integer(&value, "id").unwrap_or_default(),
            command: integer(&value, "command").unwrap_or_default(),
        }),
        "asks" => Ask::Asks,
        // What the page downloads: the engine's own transfers, which the app starts, answers,
        // stops and resumes by the id the engine gave them.
        "start_download" => Ask::Command(WebViewCommand::StartDownload {
            uri: text(&value, "url").unwrap_or_default(),
        }),
        "answer_download" => Ask::Command(WebViewCommand::DownloadDecision {
            id: integer(&value, "id").unwrap_or_default(),
            allow: flag(&value, "allow"),
            by: tilo_webview::AnsweredBy::Person,
        }),
        "cancel_download" => Ask::Command(WebViewCommand::CancelNativeDownload {
            id: integer(&value, "id").unwrap_or_default(),
        }),
        "retry_download" => Ask::Command(WebViewCommand::RetryDownload {
            id: integer(&value, "id").unwrap_or_default(),
        }),
        "cancel_all_downloads" => Ask::Command(WebViewCommand::CancelAllDownloads),
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
            if let Some(image) = &reply.image {
                answer["image"] = base64::engine::general_purpose::STANDARD.encode(image).into();
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
    let downloads: Vec<serde_json::Value> = asks
        .downloads
        .iter()
        .map(|download| {
            serde_json::json!({
                "id": download.id,
                "state": &download.state,
                "name": &download.name,
                "url": &download.url,
                "path": &download.path,
                "retryable": download.retryable,
                "received": download.received,
                "total": download.total,
                "error": &download.error,
            })
        })
        .collect();
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
        "downloads": downloads,
    })
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
    value.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
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
    value.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false)
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
