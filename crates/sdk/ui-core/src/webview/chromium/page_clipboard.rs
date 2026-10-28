//! The page's clipboard where the engine has none of its own (Linux): the
//! host sends what a page copied (`clipboard`) and asks for the system
//! clipboard's text when a page pastes or reads (`clipboard_read`), which
//! the embedder answers (`clipboard_contents`). The host's
//! `page_clipboard.h` says when.

use super::{Value, WebViewEvent};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

/// The most a copy carries, as the host bounds it: its text, and its markup,
/// which the host drops from a copy that has more.
const MAX_TEXT: usize = 4 * 1024 * 1024;
const MAX_HTML: usize = 1024 * 1024;

/// Whether the host bridges this tab's clipboard (`page_clipboard` in its
/// `ready`), whether the tab asked it to
/// (`WebViewEngine::share_clipboard`), and whether the host
/// was told. A tab that never asks sends the page's copies nowhere and
/// leaves its pastes to the engine, so an embedder that does not answer
/// reads never makes a page wait.
#[derive(Default)]
pub(super) struct Sharing {
    pub(super) can: AtomicBool,
    pub(super) asked: AtomicBool,
    told: AtomicBool,
}

impl Sharing {
    /// Tell the host once it can and the tab asked; `push` sends the
    /// command, and says whether it was queued.
    pub(super) fn tell(&self, push: impl FnOnce(Value) -> bool) {
        if !self.asked.load(Ordering::Acquire)
            || !self.can.load(Ordering::Acquire)
            || self.told.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if !push(json!({"op": "page_clipboard"})) {
            self.told.store(false, Ordering::Release);
        }
    }
}

/// A `clipboard` or `clipboard_read` event, taken only when every part of it
/// is what the host sends.
pub(super) fn decode(value: &Value) -> Option<WebViewEvent> {
    let fields = value.as_object()?;
    let only = |allowed: &[&str]| fields.keys().all(|key| allowed.contains(&key.as_str()));
    match value["event"].as_str()? {
        "clipboard" => {
            if !only(&["event", "tab", "text", "html", "secret"]) {
                return None;
            }
            let text = value["text"]
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= MAX_TEXT)?;
            let html = match value.get("html") {
                None => None,
                Some(html) => Some(html.as_str().filter(|html| html.len() <= MAX_HTML)?),
            };
            Some(WebViewEvent::ClipboardWrite {
                text: text.to_owned(),
                html: html.filter(|html| !html.is_empty()).map(str::to_owned),
                secret: value["secret"].as_bool()?,
            })
        }
        "clipboard_read" => {
            if !only(&["event", "tab", "request_id"]) {
                return None;
            }
            let request_id = value["request_id"]
                .as_u64()
                .filter(|id| *id > 0)
                .and_then(|id| i32::try_from(id).ok())
                .and_then(|id| u32::try_from(id).ok())?;
            Some(WebViewEvent::ClipboardRead { request_id })
        }
        _ => None,
    }
}

/// The `clipboard_contents` command answering read `request_id`; none for an
/// id the host never sends or text the host would refuse, which the page
/// then waits out.
pub(super) fn answer(request_id: u32, text: Option<&str>) -> Option<Value> {
    if request_id == 0 || i32::try_from(request_id).is_err() {
        return None;
    }
    Some(match text.filter(|text| text.len() <= MAX_TEXT) {
        Some(text) => json!({"op": "clipboard_contents", "request_id": request_id, "text": text}),
        None => json!({"op": "clipboard_contents", "request_id": request_id}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_taken_with_its_markup_and_whether_it_is_secret() {
        let event =
            decode(&json!({"event": "clipboard", "tab": 3, "text": "hunter2", "secret": true}));
        assert!(
            matches!(event, Some(WebViewEvent::ClipboardWrite { ref text, html: None, secret: true }) if text == "hunter2")
        );
        let event = decode(
            &json!({"event": "clipboard", "tab": 3, "text": "a link", "html": "<a href=\"/x\">a link</a>", "secret": false}),
        );
        assert!(
            matches!(event, Some(WebViewEvent::ClipboardWrite { html: Some(ref html), secret: false, .. }) if html.starts_with("<a "))
        );
    }

    #[test]
    fn a_copy_the_host_would_not_send_is_refused() {
        for value in [
            json!({"event": "clipboard", "text": "", "secret": false}),
            json!({"event": "clipboard", "text": "x"}),
            json!({"event": "clipboard", "text": "x", "secret": false, "extra": 1}),
            json!({"event": "clipboard", "text": "x".repeat(MAX_TEXT + 1), "secret": false}),
            json!({"event": "clipboard_read", "request_id": 0}),
            json!({"event": "clipboard_read", "request_id": 1_u64 << 31}),
        ] {
            assert!(decode(&value).is_none(), "{value}");
        }
    }

    #[test]
    fn a_read_is_answered_with_the_text_or_nothing() {
        assert_eq!(
            answer(7, Some("pasted")),
            Some(json!({"op": "clipboard_contents", "request_id": 7, "text": "pasted"}))
        );
        assert_eq!(
            answer(7, None),
            Some(json!({"op": "clipboard_contents", "request_id": 7}))
        );
        assert_eq!(answer(0, Some("x")), None);
    }
}
