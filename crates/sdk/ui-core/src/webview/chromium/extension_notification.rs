//! An extension's notification (`chrome.notifications`) as the host tells
//! the Browser of it.

use serde_json::Value;

use crate::webview::WebViewEvent;

/// An extension's notification as the host tells of it
/// (`extension_notifications.h`): what it shows, or that it is gone. Its text
/// is bounded as the host bounds it.
pub(super) fn decode(id: &str, value: &Value) -> Option<WebViewEvent> {
    const TEXT: usize = 4096;
    let notification = value["notification"]
        .as_str()
        .filter(|notification| notification.len() <= 500)?;
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| text.len() <= TEXT)
            .map(str::to_owned)
    };
    let notice = if value["cleared"] == true {
        None
    } else {
        let items = value["items"]
            .as_array()
            .filter(|items| items.len() <= 16)?
            .iter()
            .map(|item| Some((text(&item["title"])?, text(&item["message"])?)))
            .collect::<Option<Vec<_>>>()?;
        let buttons = value["buttons"]
            .as_array()
            .filter(|buttons| buttons.len() <= 2)?
            .iter()
            .map(text)
            .collect::<Option<Vec<_>>>()?;
        let progress = match value.get("progress") {
            None => None,
            Some(progress) => Some(
                progress
                    .as_u64()
                    .filter(|progress| *progress <= 100)
                    .and_then(|progress| u8::try_from(progress).ok())?,
            ),
        };
        Some(crate::webview::ExtensionNotice {
            title: text(&value["title"])?,
            message: text(&value["message"])?,
            context: text(&value["context"])?,
            items,
            buttons,
            progress,
            silent: value["silent"] == true,
        })
    };
    Some(WebViewEvent::ExtensionNotification {
        id: id.into(),
        notification: notification.into(),
        notice,
    })
}
