//! Decoding the host's events that carry what a page, a site or an
//! extension said: each is taken only when every part of it is what the
//! host sends.

use super::{
    Value, WebViewEvent, decode_base64_bounded, extension_id, extension_notification,
    extension_omnibox, extension_sign_in, valid_id, wire_u64,
};

pub(super) fn valid_site_event(value: &Value, event: &str) -> bool {
    let header = value["request_id"]
        .as_u64()
        .is_some_and(|n| n > 0 && i32::try_from(n).is_ok())
        && value["origin"].as_str().is_some_and(|s| s.len() <= 8192)
        && value["generation"].as_f64().is_some_and(|n| {
            n.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0
        });
    header
        && if event == "site_result" {
            value["success"].is_boolean()
                && value["action"].as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "status" | "clear" | "revoke" | "reset" | "enable" | "disable" | "reload"
                    )
                })
                && value["message"].as_str().is_some_and(|s| s.len() <= 1024)
        } else {
            ["global_enabled", "excepted", "effective"]
                .iter()
                .all(|k| value[*k].is_boolean())
                && value["blocked"].as_f64().is_some_and(|n| {
                    n.is_finite()
                        && (0.0..=9_007_199_254_740_991.0).contains(&n)
                        && n.fract() == 0.0
                })
                && matches!(
                    value["security"].as_str(),
                    Some("insecure" | "secure" | "unverified")
                )
                && ["subject", "issuer"]
                    .iter()
                    .all(|k| value[*k].as_str().is_some_and(|s| s.len() <= 512))
                && ["certificate_status", "tls_version", "content_status"]
                    .iter()
                    .all(|k| value[*k].as_u64().is_some_and(|n| u32::try_from(n).is_ok()))
                && value["expires"].as_f64().is_some_and(f64::is_finite)
                && value["permissions"].as_object().is_some_and(|p| {
                    p.len() == 4
                        && ["camera", "microphone", "location", "notifications"]
                            .iter()
                            .all(|k| {
                                matches!(
                                    p.get(*k).and_then(Value::as_str),
                                    Some("allowed" | "blocked" | "ask")
                                )
                            })
                })
        }
}

/// An `extension_action`, `extension_open_popup`, `extension_execute_action`,
/// `extension_notification`, `extension_sign_in`, `extension_omnibox_*` or
/// `extension_side_panel`
/// event, when every part of it is what the host sends: the extension's id,
/// its own pages or a web page to sign in on, bounded text and an icon of at
/// most 128 pixels a side.
pub(super) fn decode_extension_event(value: &Value) -> Option<WebViewEvent> {
    // Absent: unchanged. Null: taken back. Otherwise a value `read` accepts,
    // or the event is refused.
    macro_rules! field {
        ($change:ident, $key:literal, $read:expr) => {
            match $change.get($key) {
                None => None,
                Some(Value::Null) => Some(None),
                Some(value) => Some(Some($read(value)?)),
            }
        };
    }
    let id = value["id"].as_str().filter(|id| extension_id(id))?;
    let own = |url: &str| {
        url.len() <= 2048
            && url
                .strip_prefix("chrome-extension://")
                .and_then(|rest| rest.strip_prefix(id))
                .is_some_and(|rest| rest.starts_with('/'))
    };
    if value["event"] == "extension_execute_action" {
        return Some(WebViewEvent::ExtensionExecuteAction { id: id.into() });
    }
    if value["event"] == "extension_notification" {
        return extension_notification::decode(id, value);
    }
    if value["event"] == "extension_sign_in" {
        return extension_sign_in::decode(id, value);
    }
    if value["event"]
        .as_str()
        .is_some_and(|event| event.starts_with("extension_omnibox_"))
    {
        return extension_omnibox::decode(id, value);
    }
    if value["event"] == "extension_open_popup" {
        let url = value["url"].as_str().filter(|url| own(url))?;
        return Some(WebViewEvent::ExtensionOpenPopup {
            id: id.into(),
            url: url.into(),
        });
    }
    if value["event"] == "extension_side_panel" {
        use crate::webview::ExtensionSidePanelChange as Change;
        let url = match &value["url"] {
            Value::Null => None,
            url => Some(url.as_str().filter(|url| own(url))?.to_owned()),
        };
        let change = match (value["mode"].as_str()?, url) {
            ("open", Some(url)) => Change::Open(url),
            ("toggle", Some(url)) => Change::Toggle(url),
            ("options", url) => Change::Options(url),
            ("close", None) => Change::Close,
            _ => return None,
        };
        return Some(WebViewEvent::ExtensionSidePanel {
            id: id.into(),
            change,
        });
    }
    let change = value["change"].as_object()?;
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| text.len() <= 1024)
            .map(str::to_owned)
    };
    let icon = |value: &Value| {
        if let Some(path) = value["path"].as_str() {
            return (path.starts_with('/') && path.len() <= 2048)
                .then(|| crate::webview::ExtensionActionIcon::Path(path.into()));
        }
        let side = |key: &str| {
            u32::try_from(value[key].as_u64()?)
                .ok()
                .filter(|side| (1..=128).contains(side))
        };
        let (width, height) = (side("width")?, side("height")?);
        let length = usize::try_from(width.checked_mul(height)?.checked_mul(4)?).ok()?;
        let rgba = decode_base64_bounded(value["data"].as_str()?, length)?;
        (rgba.len() == length).then_some(crate::webview::ExtensionActionIcon::Pixels {
            width,
            height,
            rgba,
        })
    };
    let popup = |value: &Value| {
        value
            .as_str()
            .filter(|url| url.is_empty() || own(url))
            .map(str::to_owned)
    };
    if change.keys().any(|key| {
        !matches!(
            key.as_str(),
            "badge" | "title" | "popup" | "enabled" | "icon"
        )
    }) {
        return None;
    }
    Some(WebViewEvent::ExtensionAction {
        id: id.into(),
        global: value["global"].as_bool()?,
        change: crate::webview::ExtensionActionChange {
            badge: field!(change, "badge", text),
            title: field!(change, "title", text),
            popup: field!(change, "popup", popup),
            enabled: field!(change, "enabled", Value::as_bool),
            icon: field!(change, "icon", icon),
        },
    })
}

pub(super) fn decode_site_event(value: &Value, event: &str) -> Option<WebViewEvent> {
    Some(match event {
        "site_status" => WebViewEvent::SiteStatus(crate::webview::SiteStatus {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            blocked: wire_u64(&value["blocked"])?,
            global_enabled: value["global_enabled"].as_bool()?,
            excepted: value["excepted"].as_bool()?,
            effective: value["effective"].as_bool()?,
            security: value["security"].as_str()?.into(),
            certificate_status: u32::try_from(value["certificate_status"].as_u64()?).ok()?,
            tls_version: u32::try_from(value["tls_version"].as_u64()?).ok()?,
            content_status: u32::try_from(value["content_status"].as_u64()?).ok()?,
            subject: value["subject"].as_str()?.into(),
            issuer: value["issuer"].as_str()?.into(),
            expires: value["expires"].as_f64()?,
            permissions: value["permissions"]
                .as_object()?
                .iter()
                .map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
                .collect::<Option<Vec<_>>>()?,
            can_reset: value["can_reset"].as_bool().unwrap_or(false),
        }),
        "site_result" => WebViewEvent::SiteResult {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            action: value["action"].as_str()?.into(),
            success: value["success"].as_bool()?,
            message: value["message"].as_str()?.into(),
        },
        _ => return None,
    })
}

/// The longest text of a page's question the host sends, in bytes.
const MAX_PAGE_DIALOG_TEXT: usize = 16 * 1024;

/// A `js_dialog` event: the page's question, its texts within the host's
/// bounds.
pub(super) fn decode_page_dialog(value: &Value) -> Option<crate::webview::PageDialog> {
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| text.len() <= MAX_PAGE_DIALOG_TEXT)
            .map(str::to_owned)
    };
    let kind = match value["kind"].as_str()? {
        "alert" => crate::webview::PageDialogKind::Alert,
        "confirm" => crate::webview::PageDialogKind::Confirm,
        "prompt" => crate::webview::PageDialogKind::Prompt,
        "beforeunload" => crate::webview::PageDialogKind::Leave {
            reload: value["reload"].as_bool()?,
        },
        _ => return None,
    };
    Some(crate::webview::PageDialog {
        id: u32::try_from(value["id"].as_u64()?)
            .ok()
            .filter(|_| valid_id(value))?,
        kind,
        origin: value["origin"]
            .as_str()
            .filter(|origin| origin.len() <= 8192)?
            .to_owned(),
        message: text("message")?,
        default_text: text("default_text")?,
    })
}

/// An `extension_shortcuts` event: the keys extensions' commands hold, each
/// a key as Chrome names it with its modifiers, at most 256.
pub(super) fn decode_extension_shortcuts(
    value: &Value,
) -> Option<Vec<crate::webview::ExtensionShortcut>> {
    const NAMED: [&str; 13] = [
        "Comma", "Period", "Space", "Home", "End", "PageUp", "PageDown", "Delete", "Tab", "Up",
        "Down", "Left", "Right",
    ];
    let list = value["shortcuts"]
        .as_array()
        .filter(|list| list.len() <= 256)?;
    list.iter()
        .map(|entry| {
            let key = entry["key"].as_str().filter(|key| {
                (key.len() == 1
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
                    || NAMED.contains(key)
            })?;
            let flag = |name: &str| entry[name].as_bool();
            Some(crate::webview::ExtensionShortcut {
                key: key.to_owned(),
                modifiers: crate::webview::KeyModifiers {
                    ctrl: flag("ctrl")?,
                    shift: flag("shift")?,
                    alt: flag("alt")?,
                    meta: flag("meta")?,
                },
            })
        })
        .collect()
}
